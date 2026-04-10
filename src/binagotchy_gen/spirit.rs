// Spirit mode post-processing module
use super::binagotchy_utils::*;
use super::types::*;
use image::RgbaImage;
use rand::Rng;

struct SpiritLayers {
    aura: RgbaImage,
    body: RgbaImage,
    glow2: RgbaImage,
    glow1: RgbaImage,
    line_layer: RgbaImage,
}

pub fn apply_spirit_postprocess<R: Rng>(
    cat_rgba: &RgbaImage,
    rng: &mut R,
    inner_rgb: (u8, u8, u8),
    outer_rgb: (u8, u8, u8),
) -> RgbaImage {
    let (w, h) = cat_rgba.dimensions();
    let bg = spirit_background_cached(w as usize, h as usize, inner_rgb, outer_rgb);
    let layers = build_spirit_layers(cat_rgba);
    let spark = create_sparkles(w, h, rng);

    let mut composite = bg;
    composite = alpha_composite(&composite, &layers.aura);
    composite = alpha_composite(&composite, &layers.body);
    composite = alpha_composite(&composite, &layers.glow2);
    composite = alpha_composite(&composite, &layers.glow1);
    composite = alpha_composite(&composite, &layers.line_layer);
    composite = alpha_composite(&composite, &spark);
    composite
}

fn build_spirit_layers(cat_rgba: &RgbaImage) -> SpiritLayers {
    build_spirit_layers_with_effects(cat_rgba, 6.0, 14.0, 18.0, 0.52)
}

fn build_spirit_layers_with_effects(
    cat_rgba: &RgbaImage,
    glow1_radius: f32,
    glow2_radius: f32,
    aura_radius: f32,
    aura_scale: f32,
) -> SpiritLayers {
    let (w, h) = cat_rgba.dimensions();
    let alpha = extract_alpha(cat_rgba);
    let line_mask = line_mask_from_exact_colors(cat_rgba);
    let body_mask = subtract(&alpha, &line_mask);

    let body_color = (135, 190, 245, 125);
    let mut body = RgbaImage::from_pixel(w, h, color_to_rgba(body_color));
    let ramp = create_vertical_ramp(w, h);
    let body_alpha = multiply_masks(&body_mask, &ramp);
    let body_alpha_blurred = gaussian_blur_alpha(&body_alpha, 0.6);
    set_alpha_channel(&mut body, &body_alpha_blurred);

    let line_rgb = (244, 250, 255, 255);
    let mut line_layer = RgbaImage::from_pixel(w, h, color_to_rgba(line_rgb));
    set_alpha_channel(&mut line_layer, &line_mask);

    let mut glow1 = line_layer.clone();
    let glow1_alpha = gaussian_blur_alpha(&line_mask, glow1_radius);
    set_alpha_channel(&mut glow1, &glow1_alpha);

    let mut glow2 = line_layer.clone();
    let glow2_alpha = gaussian_blur_alpha(&line_mask, glow2_radius);
    set_alpha_channel(&mut glow2, &glow2_alpha);

    let aura_color = (175, 220, 255, 255);
    let mut aura = RgbaImage::from_pixel(w, h, color_to_rgba(aura_color));
    let aura_alpha = gaussian_blur_alpha(&alpha, aura_radius);
    let aura_alpha_scaled = scale_alpha(&aura_alpha, aura_scale);
    set_alpha_channel(&mut aura, &aura_alpha_scaled);

    SpiritLayers {
        aura,
        body,
        glow2,
        glow1,
        line_layer,
    }
}

fn spirit_background_cached(
    w: usize,
    h: usize,
    inner_rgb: (u8, u8, u8),
    outer_rgb: (u8, u8, u8),
) -> RgbaImage {
    let center = (w as f32 * 0.60, h as f32 * 0.36);
    let mut bg = make_radial_gradient_rgb((w, h), inner_rgb, outer_rgb, center, 1.45);
    let vignette = create_vignette(w, h, 0.26);
    bg = alpha_composite(&bg, &vignette);
    bg
}

fn make_radial_gradient_rgb(
    size: (usize, usize),
    inner_rgb: (u8, u8, u8),
    outer_rgb: (u8, u8, u8),
    center: (f32, f32),
    power: f32,
) -> RgbaImage {
    let (w, h) = size;
    let (cx, cy) = center;
    let mut img = RgbaImage::from_pixel(
        w as u32,
        h as u32,
        rgba(outer_rgb.0, outer_rgb.1, outer_rgb.2, 255),
    );
    let maxd = ((cx.max(w as f32 - cx)).powi(2) + (cy.max(h as f32 - cy)).powi(2)).sqrt();

    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / maxd;
            let t = d.powf(power).clamp(0.0, 1.0);
            let r = (inner_rgb.0 as f32 * (1.0 - t) + outer_rgb.0 as f32 * t) as u8;
            let g = (inner_rgb.1 as f32 * (1.0 - t) + outer_rgb.1 as f32 * t) as u8;
            let b = (inner_rgb.2 as f32 * (1.0 - t) + outer_rgb.2 as f32 * t) as u8;
            img.put_pixel(x as u32, y as u32, rgba(r, g, b, 255));
        }
    }

    img
}

fn create_vignette(w: usize, h: usize, strength: f32) -> RgbaImage {
    let mut vignette = RgbaImage::from_pixel(w as u32, h as u32, rgba(0, 0, 0, 0));
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let maxd = (cx.powi(2) + cy.powi(2)).sqrt();

    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / maxd;
            let v = (255.0 * d.powf(2.1).min(1.0) * strength) as u8;
            vignette.put_pixel(x as u32, y as u32, rgba(0, 0, 0, v));
        }
    }

    vignette
}

fn line_mask_from_exact_colors(cat_rgba: &RgbaImage) -> RgbaImage {
    let (w, h) = cat_rgba.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, rgba(0, 0, 0, 0));
    let line_colors = [(20, 20, 20), (10, 10, 10), (35, 35, 40)];

    for y in 0..h {
        for x in 0..w {
            let pixel = cat_rgba.get_pixel(x, y);
            if pixel[3] == 0 {
                continue;
            }
            let rgb = (pixel[0], pixel[1], pixel[2]);
            if line_colors.contains(&rgb) {
                out.put_pixel(x, y, rgba(255, 255, 255, 255));
            }
        }
    }

    out
}

fn extract_alpha(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut alpha = RgbaImage::from_pixel(w, h, rgba(0, 0, 0, 0));

    for y in 0..h {
        for x in 0..w {
            let a = img.get_pixel(x, y)[3];
            alpha.put_pixel(x, y, rgba(a, a, a, 255));
        }
    }

    alpha
}

fn create_vertical_ramp(w: u32, h: u32) -> RgbaImage {
    let mut ramp = RgbaImage::from_pixel(w, h, rgba(0, 0, 0, 0));

    for y in 0..h {
        let t = y as f32 / h.max(1) as f32;
        let val = (105.0 + 70.0 * (1.0 - (t - 0.55).abs() * 1.55)).clamp(0.0, 255.0) as u8;
        for x in 0..w {
            ramp.put_pixel(x, y, rgba(val, val, val, 255));
        }
    }

    ramp
}

fn multiply_masks(a: &RgbaImage, b: &RgbaImage) -> RgbaImage {
    let (w, h) = a.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, rgba(0, 0, 0, 0));

    for y in 0..h {
        for x in 0..w {
            let av = a.get_pixel(x, y)[0] as f32 / 255.0;
            let bv = b.get_pixel(x, y)[0] as f32 / 255.0;
            let v = (av * bv * 255.0) as u8;
            out.put_pixel(x, y, rgba(v, v, v, 255));
        }
    }

    out
}

fn gaussian_blur_alpha(img: &RgbaImage, radius: f32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let r = radius.ceil() as i32;
    let mut out = img.clone();

    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut sum = 0.0;
            let mut count = 0.0;
            for dx in -r..=r {
                let nx = x + dx;
                if nx >= 0 && nx < w as i32 {
                    let val = img.get_pixel(nx as u32, y as u32)[0] as f32;
                    let weight = gaussian_weight(dx as f32, radius);
                    sum += val * weight;
                    count += weight;
                }
            }
            let val = if count > 0.0 { (sum / count) as u8 } else { 0 };
            out.put_pixel(x as u32, y as u32, rgba(val, val, val, 255));
        }
    }

    let temp = out.clone();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut sum = 0.0;
            let mut count = 0.0;
            for dy in -r..=r {
                let ny = y + dy;
                if ny >= 0 && ny < h as i32 {
                    let val = temp.get_pixel(x as u32, ny as u32)[0] as f32;
                    let weight = gaussian_weight(dy as f32, radius);
                    sum += val * weight;
                    count += weight;
                }
            }
            let val = if count > 0.0 { (sum / count) as u8 } else { 0 };
            out.put_pixel(x as u32, y as u32, rgba(val, val, val, 255));
        }
    }

    out
}

fn gaussian_weight(x: f32, sigma: f32) -> f32 {
    (-(x * x) / (2.0 * sigma * sigma)).exp()
}

fn scale_alpha(img: &RgbaImage, scale: f32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = img.clone();

    for y in 0..h {
        for x in 0..w {
            let v = img.get_pixel(x, y)[0];
            let scaled = (v as f32 * scale).min(255.0) as u8;
            out.put_pixel(x, y, rgba(scaled, scaled, scaled, 255));
        }
    }

    out
}

fn set_alpha_channel(img: &mut RgbaImage, alpha: &RgbaImage) {
    let (w, h) = img.dimensions();
    for y in 0..h {
        for x in 0..w {
            let mut pixel = *img.get_pixel(x, y);
            pixel[3] = alpha.get_pixel(x, y)[0];
            img.put_pixel(x, y, pixel);
        }
    }
}

fn create_sparkles<R: Rng>(w: u32, h: u32, rng: &mut R) -> RgbaImage {
    let mut spark = RgbaImage::from_pixel(w, h, rgba(0, 0, 0, 0));

    for _ in 0..90 {
        let radius = rng.gen_range(3..=12);
        let x = (w as f32 * 0.10 + rng.gen_range(0.0..1.0) * w as f32 * 0.80) as i32;
        let y = (h as f32 * 0.05 + rng.gen_range(0.0..1.0) * h as f32 * 0.90) as i32;
        let colors = [
            (255, 255, 255, rng.gen_range(28..=80)),
            (205, 235, 255, rng.gen_range(24..=75)),
            (190, 210, 255, rng.gen_range(22..=65)),
            (255, 240, 220, rng.gen_range(18..=60)),
        ];
        let color = colors[rng.gen_range(0..colors.len())];

        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx * dx + dy * dy > radius * radius {
                    continue;
                }
                let px = x + dx;
                let py = y + dy;
                if px >= 0 && px < w as i32 && py >= 0 && py < h as i32 {
                    spark.put_pixel(px as u32, py as u32, color_to_rgba(color));
                }
            }
        }
    }

    spark
}

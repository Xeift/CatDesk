// Binagotchy character generation - integrated modules
mod binagotchy_utils;
mod character;
pub mod constants;
pub mod eye_animation;
mod headwear;
mod spirit;
mod types;

// use types::*;
use constants::*;

use image::RgbaImage;
use rand::Rng;
// use rand::SeedableRng;
use rand_mt::Mt19937GenRand32;
use std::collections::HashMap;

const SPIRIT_FRAME_SCALE: u32 = 2;

/// Main entry point - creates a complete character with all features
/// Returns (image, traits_map)
pub fn create_character(
    seed: Option<u64>,
    canvas: u32,
    upscale: u32,
    eyes: &str,
    headwear: &str,
    spirit_prob: f32,
    eye_openness: f32,
    tail_state: i32,
) -> (RgbaImage, HashMap<String, String>) {
    let seed_val = seed.unwrap_or_else(|| rand::random::<u64>() % 1_000_000_000);
    let key = mt_key_from_seed(seed_val);
    let mut rng = Mt19937GenRand32::new_with_key(key.clone());
    // Keep headwear selection stable even if sprite generation changes its random consumption.
    let mut rng_stable = Mt19937GenRand32::new_with_key(key);

    // Generate base character sprite (Uses unstable RNG because mask changes affect consumption)
    let (base_img, fur_color, eye_mode) =
        character::render_base_sprite(canvas, &mut rng, eyes, eye_openness, tail_state);

    // Get mask and head box
    let (_mask, head_box) = character::mask_sitting_cat(canvas, &mut rng, tail_state);

    // Resolve headwear (Use Stable RNG)
    let headwear_name = character::resolve_headwear(&mut rng_stable, headwear);

    // Generate headwear layer if needed (Use Stable RNG for rendering if possible, but consistent choice matters most)
    let headwear_up = if headwear_name != "none" {
        let accent = (120, 170, 220, 255);
        let headwear_layer = headwear::render_headwear_layer(
            canvas,
            head_box,
            &mut rng_stable,
            &headwear_name,
            accent,
        );
        Some(binagotchy_utils::upscale_nearest(&headwear_layer, upscale))
    } else {
        None
    };

    // Upscale sprite
    let sprite = binagotchy_utils::upscale_nearest(&base_img, upscale);
    let spirit_probability = spirit_prob.clamp(0.0, 100.0) / 100.0;
    let use_spirit = spirit_probability > 0.0 && rng_stable.gen_bool(spirit_probability as f64);

    let (final_img, headwear_used, special) = if use_spirit {
        let spirit_base = center_sprite_on_canvas(&sprite, SPIRIT_FRAME_SCALE);
        let spirit_img = spirit::apply_spirit_postprocess(
            &spirit_base,
            &mut rng_stable,
            (185, 225, 255),
            (110, 170, 240),
        );
        (spirit_img, "none".to_string(), "spirit".to_string())
    } else {
        (
            compose_character_frame(
                &sprite,
                headwear_up.as_ref(),
                HEADROOM_TOP.max(0) as u32 * upscale,
            ),
            headwear_name.clone(),
            "normal".to_string(),
        )
    };

    // Build traits map
    let mut traits = HashMap::new();
    traits.insert("fur".to_string(), fur_name(fur_color));
    traits.insert("eyes".to_string(), eye_mode);
    traits.insert("headwear".to_string(), headwear_used);
    traits.insert("special".to_string(), special);

    (final_img, traits)
}

pub fn apply_mascot_spirit_frame(
    seed: u64,
    sprite: &RgbaImage,
    frame_width: u32,
    frame_height: u32,
) -> RgbaImage {
    let mut rng = Mt19937GenRand32::new_with_key(mt_key_from_seed(seed));
    let spirit_base = center_sprite_on_frame(sprite, frame_width, frame_height);
    spirit::apply_spirit_postprocess(&spirit_base, &mut rng, (185, 225, 255), (110, 170, 240))
}

fn mt_key_from_seed(seed_val: u64) -> Vec<u32> {
    if seed_val >> 32 == 0 {
        vec![seed_val as u32]
    } else {
        vec![seed_val as u32, (seed_val >> 32) as u32]
    }
}

fn fur_name(c: types::Color) -> String {
    for (name, _, color) in FUR_TRAITS.iter() {
        if color == &c {
            return name.to_string();
        }
    }
    "custom".to_string()
}

fn center_sprite_on_canvas(sprite: &RgbaImage, scale: u32) -> RgbaImage {
    let frame_scale = scale.max(1);
    let (sprite_width, sprite_height) = sprite.dimensions();
    let frame_width = sprite_width * frame_scale;
    let frame_height = sprite_height * frame_scale;
    center_sprite_on_frame(sprite, frame_width, frame_height)
}

fn center_sprite_on_frame(sprite: &RgbaImage, frame_width: u32, frame_height: u32) -> RgbaImage {
    let (sprite_width, sprite_height) = sprite.dimensions();
    let frame_width = frame_width.max(sprite.width());
    let frame_height = frame_height.max(sprite.height());
    let mut frame = RgbaImage::from_pixel(frame_width, frame_height, types::rgba(0, 0, 0, 0));
    let offset_x = (frame_width - sprite_width) / 2;
    let offset_y = (frame_height - sprite_height) / 2;
    image::imageops::overlay(&mut frame, sprite, offset_x as i64, offset_y as i64);
    frame
}

fn compose_character_frame(
    sprite: &RgbaImage,
    headwear_up: Option<&RgbaImage>,
    headroom_px: u32,
) -> RgbaImage {
    let (sw, sh) = sprite.dimensions();
    let mut frame_size = sw.max(sh.saturating_add(headroom_px));
    if let Some(headwear_up) = headwear_up {
        frame_size = frame_size
            .max(headwear_up.width())
            .max(headwear_up.height());
    }

    let mut frame = RgbaImage::from_pixel(frame_size, frame_size, types::rgba(0, 0, 0, 0));
    let sprite_x = (frame_size - sw) / 2;
    let sprite_y = frame_size - sh;

    image::imageops::overlay(&mut frame, sprite, sprite_x as i64, sprite_y as i64);

    if let Some(headwear_up) = headwear_up {
        overlay_headwear_on_frame(&mut frame, sprite, headwear_up);
    }

    frame
}

fn overlay_headwear_on_frame(frame: &mut RgbaImage, sprite: &RgbaImage, headwear_up: &RgbaImage) {
    let (sw, sh) = sprite.dimensions();
    let (bw, bh) = frame.dimensions();
    let (_, hw_h) = headwear_up.dimensions();

    let headroom_px = hw_h.saturating_sub(sh);
    let offset_x = (bw - sw) / 2;
    let offset_y = bh - sh - headroom_px;

    image::imageops::overlay(frame, headwear_up, offset_x as i64, offset_y as i64);
}

#[cfg(test)]
mod tests {
    use super::{apply_mascot_spirit_frame, create_character};

    #[test]
    fn spirit_mode_disables_headwear() {
        let (_image, traits) =
            create_character(Some(42), 32, 8, "normal", "top_hat", 100.0, 1.0, 0);

        assert_eq!(traits.get("headwear").map(String::as_str), Some("none"));
        assert_eq!(traits.get("special").map(String::as_str), Some("spirit"));
    }

    #[test]
    fn mascot_spirit_frame_fills_background() {
        let (image, _) = create_character(Some(42), 32, 1, "normal", "none", 0.0, 1.0, 0);
        let spirit = apply_mascot_spirit_frame(42, &image, 40, 32);
        let (width, height) = spirit.dimensions();
        assert_eq!(
            (width, height),
            (image.width().max(40), image.height().max(32))
        );
        assert_eq!(spirit.get_pixel(0, 0)[3], 255);
        assert_eq!(spirit.get_pixel(width - 1, 0)[3], 255);
        assert_eq!(spirit.get_pixel(0, height - 1)[3], 255);
    }

    #[test]
    fn non_spirit_frame_size_stays_constant_with_headwear() {
        let (without_headwear, _) =
            create_character(Some(42), 32, 1, "normal", "none", 0.0, 1.0, 0);
        let (with_headwear, _) =
            create_character(Some(42), 32, 1, "normal", "top_hat", 0.0, 1.0, 0);
        assert_eq!(without_headwear.dimensions(), with_headwear.dimensions());
    }
}

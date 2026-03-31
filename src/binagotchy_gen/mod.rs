// Binagotchy character generation - integrated modules
mod binagotchy_utils;
mod character;
pub mod constants;
pub mod eye_animation;
mod headwear;
mod types;

// use types::*;
use constants::*;

use image::RgbaImage;
// use rand::SeedableRng;
use rand_mt::Mt19937GenRand32;
use std::collections::HashMap;

/// Main entry point - creates a complete character with all features
/// Returns (image, traits_map)
pub fn create_character(
    seed: Option<u64>,
    canvas: u32,
    upscale: u32,
    eyes: &str,
    headwear: &str,
    eye_openness: f32,
    tail_state: i32,
) -> (RgbaImage, HashMap<String, String>) {
    let seed_val = seed.unwrap_or_else(|| rand::random::<u64>() % 1_000_000_000);
    // Python compatibility seeding: split u64 into u32 chunks (little endian)
    let key = if seed_val >> 32 == 0 {
        vec![seed_val as u32]
    } else {
        vec![seed_val as u32, (seed_val >> 32) as u32]
    };
    let mut rng = Mt19937GenRand32::new_with_key(key.clone());
    // Keep headwear selection stable even if sprite generation changes its random consumption.
    let mut rng_stable = Mt19937GenRand32::new_with_key(key);

    // Generate base character sprite (Uses unstable RNG because mask changes affect consumption)
    let (base_img, fur_color, eye_mode) = character::render_base_sprite(
        canvas,
        &mut rng,
        eyes,
        eye_openness,
        tail_state,
    );

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
    let final_img = compose_character_frame(&sprite, headwear_up.as_ref());

    // Build traits map
    let mut traits = HashMap::new();
    traits.insert("fur".to_string(), fur_name(fur_color));
    traits.insert("eyes".to_string(), eye_mode);
    traits.insert("headwear".to_string(), headwear_name);

    (final_img, traits)
}

fn fur_name(c: types::Color) -> String {
    for (name, _, color) in FUR_TRAITS.iter() {
        if color == &c {
            return name.to_string();
        }
    }
    "custom".to_string()
}

fn compose_character_frame(sprite: &RgbaImage, headwear_up: Option<&RgbaImage>) -> RgbaImage {
    let (sw, sh) = sprite.dimensions();
    let mut frame_size = sw.max(sh);
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

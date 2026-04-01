use base64::Engine as _;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use rand::Rng;
use ratatui::{
    prelude::{Color, Style},
    text::{Line, Span},
};
use serde::Serialize;
use std::{collections::HashMap, io::Cursor};
use crate::binagotchy_gen;

const MASCOT_CANVAS: u32 = 32;
const MASCOT_UPSCALE: u32 = 1;
const MASCOT_FRAME_MS: u64 = 50;
const MASCOT_SPIRIT_PERCENT: u64 = 1;
const MASCOT_SPIRIT_FRAME_WIDTH: u32 = 40;
const MASCOT_SPIRIT_FRAME_HEIGHT: u32 = 32;
const SPIRIT_HERO_BG_WIDTH: u32 = 720;
const SPIRIT_HERO_BG_HEIGHT: u32 = 420;
const WIDGET_MASCOT_ALPHABET: &str =
    ".0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_";
const MASCOT_SEQUENCE: &[(u8, i32, u8)] = &[
    (10, 1, 7),
    (10, 0, 7),
    (10, 1, 7),
    (10, 0, 7),
    (10, 1, 2),
    (5, 1, 1),
    (0, 1, 4),
    (5, 0, 1),
    (10, 0, 6),
    (10, 1, 7),
    (10, 0, 7),
];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetMascotSequenceStep {
    pub frame: u8,
    pub repeat: u8,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetMascot {
    pub width: u32,
    pub height: u32,
    pub frame_ms: u64,
    pub spirit_hero_background: String,
    pub palette: Vec<String>,
    pub frames: Vec<String>,
    pub sequence: Vec<WidgetMascotSequenceStep>,
}

#[derive(Clone)]
pub struct TuiMascotCell {
    pub glyph: char,
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
}

#[derive(Clone)]
pub struct TuiMascotFrame {
    pub rows: Vec<Vec<TuiMascotCell>>,
}

#[derive(Clone)]
pub struct MascotPack {
    pub frame_ms: u64,
    pub tui_frames: Vec<TuiMascotFrame>,
}

impl MascotPack {
    pub fn current_tui_frame(&self, now_millis: u128) -> &TuiMascotFrame {
        let idx = if self.tui_frames.is_empty() {
            0
        } else {
            ((now_millis / self.frame_ms as u128) as usize) % self.tui_frames.len()
        };
        &self.tui_frames[idx]
    }

    pub fn required_tui_block_height(&self) -> u16 {
        let rows = self
            .tui_frames
            .iter()
            .map(|frame| frame.rows.len() as u16)
            .max()
            .unwrap_or(0);
        rows.saturating_add(2)
    }
}

pub fn build_workspace_mascot(workspace_root: &str) -> MascotPack {
    let frames = mascot_source_frames(workspace_root);
    let cropped = crop_frames(&frames);
    let tui_frames = cropped.iter().map(build_tui_frame).collect();
    MascotPack {
        frame_ms: MASCOT_FRAME_MS,
        tui_frames,
    }
}

pub fn build_widget_mascot(workspace_root: &str) -> WidgetMascot {
    let (frames, sequence, spirit_hero_background) = mascot_widget_source(workspace_root);
    let cropped = crop_frames(&frames);
    build_widget_mascot_from_frames(&cropped, sequence, spirit_hero_background)
}

pub fn render_tui_lines(frame: &TuiMascotFrame, area_height: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let target_height = area_height as usize;
    let top_padding = target_height.saturating_sub(frame.rows.len()) / 2;
    for _ in 0..top_padding {
        lines.push(Line::from(""));
    }
    for row in &frame.rows {
        let spans: Vec<Span<'static>> = row
            .iter()
            .map(|cell| {
                let mut style = Style::default();
                if let Some((r, g, b)) = cell.fg {
                    style = style.fg(Color::Rgb(r, g, b));
                }
                if let Some((r, g, b)) = cell.bg {
                    style = style.bg(Color::Rgb(r, g, b));
                }
                Span::styled(cell.glyph.to_string(), style)
            })
            .collect();
        lines.push(Line::from(spans));
    }
    while lines.len() < target_height {
        lines.push(Line::from(""));
    }
    lines
}

fn mascot_source_frames(workspace_root: &str) -> Vec<RgbaImage> {
    let seed = stable_workspace_seed(workspace_root);
    let use_spirit = mascot_use_spirit(seed);
    let headwear_pref = mascot_headwear_preference(use_spirit);
    MASCOT_SEQUENCE
        .iter()
        .flat_map(|&(eye_openness, tail_state, repeat)| {
            let (frame, _) = binagotchy_gen::create_character(
                Some(seed),
                MASCOT_CANVAS,
                MASCOT_UPSCALE,
                "normal",
                headwear_pref,
                0.0,
                openness_value(eye_openness),
                tail_state,
            );
            let frame = if use_spirit {
                binagotchy_gen::apply_mascot_spirit_frame(
                    seed,
                    &frame,
                    MASCOT_SPIRIT_FRAME_WIDTH,
                    MASCOT_SPIRIT_FRAME_HEIGHT,
                )
            } else {
                frame
            };
            std::iter::repeat_n(frame, repeat as usize)
        })
        .collect()
}

fn mascot_widget_source(
    workspace_root: &str,
) -> (Vec<RgbaImage>, Vec<WidgetMascotSequenceStep>, String) {
    let seed = stable_workspace_seed(workspace_root);
    let use_spirit = mascot_use_spirit(seed);
    let headwear_pref = mascot_headwear_preference(use_spirit);
    let mut poses: Vec<(u8, i32)> = Vec::new();
    let mut sequence = Vec::new();

    for &(eye_openness, tail_state, repeat) in MASCOT_SEQUENCE {
        let pose = (eye_openness, tail_state);
        let frame_index = poses
            .iter()
            .position(|&(eye, tail)| eye == eye_openness && tail == tail_state)
            .unwrap_or_else(|| {
                poses.push(pose);
                poses.len() - 1
            });
        sequence.push(WidgetMascotSequenceStep {
            frame: frame_index as u8,
            repeat,
        });
    }

    let frames = poses
        .into_iter()
        .map(|(eye_openness, tail_state)| {
            let frame = binagotchy_gen::create_character(
                Some(seed),
                MASCOT_CANVAS,
                MASCOT_UPSCALE,
                "normal",
                headwear_pref,
                0.0,
                openness_value(eye_openness),
                tail_state,
            )
            .0;
            if use_spirit {
                build_spirit_subject_frame(&frame)
            } else {
                frame
            }
        })
        .collect();

    let spirit_hero_background = if use_spirit {
        build_spirit_hero_background_data_uri(seed)
    } else {
        String::new()
    };

    (frames, sequence, spirit_hero_background)
}

fn stable_workspace_seed(workspace_root: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in workspace_root.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn mascot_use_spirit(seed: u64) -> bool {
    seed % 100 < MASCOT_SPIRIT_PERCENT
}

fn mascot_headwear_preference(use_spirit: bool) -> &'static str {
    if use_spirit { "none" } else { "random" }
}

fn mt_key_from_seed(seed_val: u64) -> Vec<u32> {
    if seed_val >> 32 == 0 {
        vec![seed_val as u32]
    } else {
        vec![seed_val as u32, (seed_val >> 32) as u32]
    }
}

fn crop_frames(frames: &[RgbaImage]) -> Vec<RgbaImage> {
    let Some((frame_width, frame_height)) = frames.first().map(RgbaImage::dimensions) else {
        return Vec::new();
    };
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;

    for frame in frames {
        let (width, height) = frame.dimensions();
        for y in 0..height {
            for x in 0..width {
                if frame.get_pixel(x, y)[3] == 0 {
                    continue;
                }
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if min_x == u32::MAX {
        return frames.to_vec();
    }

    min_x = min_x.saturating_sub(2);
    min_y = min_y.saturating_sub(2);
    max_x = max_x.saturating_add(2).min(frame_width.saturating_sub(1));
    max_y = max_y.saturating_add(2).min(frame_height.saturating_sub(1));

    let width = max_x.saturating_sub(min_x).saturating_add(1);
    let height = max_y.saturating_sub(min_y).saturating_add(1);
    frames
        .iter()
        .map(|frame| image::imageops::crop_imm(frame, min_x, min_y, width, height).to_image())
        .collect()
}

fn build_spirit_hero_background_data_uri(seed: u64) -> String {
    let mut rng = rand_mt::Mt19937GenRand32::new_with_key(mt_key_from_seed(seed));
    let background = build_spirit_hero_background(
        SPIRIT_HERO_BG_WIDTH,
        SPIRIT_HERO_BG_HEIGHT,
        &mut rng,
        (185, 225, 255),
        (110, 170, 240),
    );
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(background)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encode spirit hero background png");
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn build_spirit_hero_background<R: Rng>(
    width: u32,
    height: u32,
    rng: &mut R,
    inner_rgb: (u8, u8, u8),
    outer_rgb: (u8, u8, u8),
) -> RgbaImage {
    let mut background =
        create_radial_gradient(width, height, inner_rgb, outer_rgb, (0.50, 0.24), 1.55);
    let haze = create_spirit_haze(width, height);
    background = alpha_composite_image(&background, &haze);
    let sparkles = create_spirit_scene_sparkles(width, height, rng);
    background = alpha_composite_image(&background, &sparkles);
    let vignette = create_vignette(width, height, 0.20);
    alpha_composite_image(&background, &vignette)
}

fn create_radial_gradient(
    width: u32,
    height: u32,
    inner_rgb: (u8, u8, u8),
    outer_rgb: (u8, u8, u8),
    center: (f32, f32),
    power: f32,
) -> RgbaImage {
    let (cx, cy) = (width as f32 * center.0, height as f32 * center.1);
    let maxd =
        ((cx.max(width as f32 - cx)).powi(2) + (cy.max(height as f32 - cy)).powi(2)).sqrt();
    let mut img = RgbaImage::from_pixel(width, height, Rgba([outer_rgb.0, outer_rgb.1, outer_rgb.2, 255]));
    for y in 0..height {
        for x in 0..width {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / maxd;
            let t = d.powf(power).clamp(0.0, 1.0);
            let r = (inner_rgb.0 as f32 * (1.0 - t) + outer_rgb.0 as f32 * t) as u8;
            let g = (inner_rgb.1 as f32 * (1.0 - t) + outer_rgb.1 as f32 * t) as u8;
            let b = (inner_rgb.2 as f32 * (1.0 - t) + outer_rgb.2 as f32 * t) as u8;
            img.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }
    img
}

fn create_spirit_haze(width: u32, height: u32) -> RgbaImage {
    let mut haze = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    let blobs = [
        (0.50, 0.24, 0.34, 0.26, [222, 240, 255, 54]),
        (0.24, 0.34, 0.22, 0.19, [180, 214, 248, 28]),
        (0.76, 0.32, 0.20, 0.17, [184, 220, 252, 24]),
        (0.50, 0.58, 0.28, 0.18, [144, 184, 228, 18]),
    ];
    for (cx, cy, rx, ry, color) in blobs {
        add_elliptical_glow(
            &mut haze,
            width as f32 * cx,
            height as f32 * cy,
            width as f32 * rx,
            height as f32 * ry,
            color,
        );
    }
    haze
}

fn create_spirit_scene_sparkles<R: Rng>(width: u32, height: u32, rng: &mut R) -> RgbaImage {
    let mut sparkles = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));

    for _ in 0..84 {
        let x = width as f32 * (0.18 + centered_unit(rng, 4) * 0.64);
        let y = height as f32 * (0.02 + centered_unit(rng, 5) * 0.58);
        let radius = rng.gen_range(2.2..5.6);
        let alpha = rng.gen_range(44..=86) as u8;
        let color = if rng.gen_range(0..6) == 0 {
            [255, 241, 226, alpha]
        } else {
            [236, 246, 255, alpha]
        };
        add_soft_disc(
            &mut sparkles,
            x,
            y,
            radius * 1.8,
            [color[0], color[1], color[2], alpha / 3],
        );
        add_soft_disc(&mut sparkles, x, y, radius, color);
    }

    for _ in 0..26 {
        let x = width as f32 * (0.24 + centered_unit(rng, 3) * 0.52);
        let y = height as f32 * (0.05 + centered_unit(rng, 4) * 0.42);
        let alpha = rng.gen_range(132..=210) as u8;
        let warm = rng.gen_range(0..5) == 0;
        let color = if warm {
            [255, 243, 232, alpha]
        } else {
            [244, 250, 255, alpha]
        };
        add_soft_disc(
            &mut sparkles,
            x,
            y,
            rng.gen_range(7.0..11.0),
            [color[0], color[1], color[2], alpha / 3],
        );
        add_soft_disc(
            &mut sparkles,
            x,
            y,
            rng.gen_range(1.8..3.0),
            [255, 255, 255, alpha],
        );
        add_cross_sparkle(
            &mut sparkles,
            x.round() as i32,
            y.round() as i32,
            rng.gen_range(4..=7),
            [255, 255, 255, alpha],
        );
    }

    sparkles
}

fn create_vignette(width: u32, height: u32, strength: f32) -> RgbaImage {
    let mut vignette = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let maxd = (cx.powi(2) + cy.powi(2)).sqrt();
    for y in 0..height {
        for x in 0..width {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / maxd;
            let a = (255.0 * d.powf(2.2).min(1.0) * strength) as u8;
            vignette.put_pixel(x, y, Rgba([0, 0, 0, a]));
        }
    }
    vignette
}

fn add_elliptical_glow(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    color: [u8; 4],
) {
    let min_x = (cx - rx).floor().max(0.0) as u32;
    let max_x = (cx + rx).ceil().min(img.width() as f32 - 1.0) as u32;
    let min_y = (cy - ry).floor().max(0.0) as u32;
    let max_y = (cy + ry).ceil().min(img.height() as f32 - 1.0) as u32;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = (x as f32 - cx) / rx.max(1.0);
            let dy = (y as f32 - cy) / ry.max(1.0);
            let dist = dx * dx + dy * dy;
            if dist >= 1.0 {
                continue;
            }
            let falloff = (1.0 - dist).powf(1.8);
            let alpha = (color[3] as f32 * falloff) as u8;
            blend_pixel(img, x, y, [color[0], color[1], color[2], alpha]);
        }
    }
}

fn add_soft_disc(img: &mut RgbaImage, cx: f32, cy: f32, radius: f32, color: [u8; 4]) {
    let min_x = (cx - radius).floor().max(0.0) as u32;
    let max_x = (cx + radius).ceil().min(img.width() as f32 - 1.0) as u32;
    let min_y = (cy - radius).floor().max(0.0) as u32;
    let max_y = (cy + radius).ceil().min(img.height() as f32 - 1.0) as u32;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt() / radius.max(0.001);
            if dist >= 1.0 {
                continue;
            }
            let falloff = (1.0 - dist).powf(2.3);
            let alpha = (color[3] as f32 * falloff) as u8;
            blend_pixel(img, x, y, [color[0], color[1], color[2], alpha]);
        }
    }
}

fn add_cross_sparkle(img: &mut RgbaImage, cx: i32, cy: i32, arm: i32, color: [u8; 4]) {
    for delta in -arm..=arm {
        let alpha = if delta == 0 {
            color[3]
        } else {
            ((color[3] as f32) * 0.45) as u8
        };
        blend_pixel_checked(img, cx + delta, cy, [color[0], color[1], color[2], alpha]);
        blend_pixel_checked(img, cx, cy + delta, [color[0], color[1], color[2], alpha]);
    }
}

fn blend_pixel(img: &mut RgbaImage, x: u32, y: u32, color: [u8; 4]) {
    let existing = *img.get_pixel(x, y);
    img.put_pixel(x, y, alpha_blend_rgba(existing, Rgba(color)));
}

fn blend_pixel_checked(img: &mut RgbaImage, x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return;
    }
    blend_pixel(img, x as u32, y as u32, color);
}

fn alpha_blend_rgba(base: Rgba<u8>, overlay: Rgba<u8>) -> Rgba<u8> {
    let oa = overlay[3] as f32 / 255.0;
    let ba = base[3] as f32 / 255.0;
    let out_a = oa + ba * (1.0 - oa);
    if out_a <= f32::EPSILON {
        return Rgba([0, 0, 0, 0]);
    }
    let blend_channel = |bc: u8, oc: u8| -> u8 {
        (((oc as f32 * oa) + (bc as f32 * ba * (1.0 - oa))) / out_a).round() as u8
    };
    Rgba([
        blend_channel(base[0], overlay[0]),
        blend_channel(base[1], overlay[1]),
        blend_channel(base[2], overlay[2]),
        (out_a * 255.0).round() as u8,
    ])
}

fn alpha_composite_image(base: &RgbaImage, overlay: &RgbaImage) -> RgbaImage {
    let mut out = base.clone();
    for (x, y, pixel) in overlay.enumerate_pixels() {
        if pixel[3] == 0 {
            continue;
        }
        let base_pixel = *out.get_pixel(x, y);
        out.put_pixel(x, y, alpha_blend_rgba(base_pixel, *pixel));
    }
    out
}

fn centered_unit<R: Rng>(rng: &mut R, samples: usize) -> f32 {
    let count = samples.max(1);
    let mut sum = 0.0;
    for _ in 0..count {
        sum += rng.gen_range(0.0..1.0);
    }
    sum / count as f32
}

fn build_spirit_subject_frame(sprite: &RgbaImage) -> RgbaImage {
    let cropped = crop_visible_bounds(sprite, 2);
    let (width, height) = cropped.dimensions();
    let alpha = extract_alpha_mask(&cropped);
    let line_mask = line_mask_from_exact_colors(&cropped);
    let body_mask = subtract_masks(&alpha, &line_mask);

    let mut composite = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));

    let mut body = RgbaImage::from_pixel(width, height, Rgba([135, 190, 245, 125]));
    let ramp = create_vertical_ramp(width, height);
    let body_alpha = multiply_masks(&body_mask, &ramp);
    let body_alpha_blurred = gaussian_blur_alpha(&body_alpha, 0.6);
    set_alpha_channel(&mut body, &body_alpha_blurred);
    composite = alpha_composite_image(&composite, &body);

    let mut glow2 = RgbaImage::from_pixel(width, height, Rgba([244, 250, 255, 255]));
    let glow2_alpha = gaussian_blur_alpha(&line_mask, 3.0);
    let glow2_alpha_scaled = scale_alpha(&glow2_alpha, 0.35);
    set_alpha_channel(&mut glow2, &glow2_alpha_scaled);
    composite = alpha_composite_image(&composite, &glow2);

    let mut line_layer = RgbaImage::from_pixel(width, height, Rgba([244, 250, 255, 255]));
    set_alpha_channel(&mut line_layer, &line_mask);
    composite = alpha_composite_image(&composite, &line_layer);

    place_on_frame(
        &composite,
        MASCOT_CANVAS,
        MASCOT_CANVAS,
        (MASCOT_CANVAS.saturating_sub(width)) / 2,
        (MASCOT_CANVAS.saturating_sub(height)) / 2,
    )
}

fn place_on_frame(
    sprite: &RgbaImage,
    frame_width: u32,
    frame_height: u32,
    offset_x: u32,
    offset_y: u32,
) -> RgbaImage {
    let mut frame = RgbaImage::from_pixel(frame_width, frame_height, Rgba([0, 0, 0, 0]));
    image::imageops::overlay(&mut frame, sprite, offset_x as i64, offset_y as i64);
    frame
}

fn crop_visible_bounds(sprite: &RgbaImage, padding: u32) -> RgbaImage {
    let (width, height) = sprite.dimensions();
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0;
    let mut max_y = 0;
    for y in 0..height {
        for x in 0..width {
            if sprite.get_pixel(x, y)[3] == 0 {
                continue;
            }
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x == u32::MAX {
        return sprite.clone();
    }
    let crop_x = min_x.saturating_sub(padding);
    let crop_y = min_y.saturating_sub(padding);
    let crop_max_x = max_x.saturating_add(padding).min(width.saturating_sub(1));
    let crop_max_y = max_y.saturating_add(padding).min(height.saturating_sub(1));
    image::imageops::crop_imm(
        sprite,
        crop_x,
        crop_y,
        crop_max_x.saturating_sub(crop_x).saturating_add(1),
        crop_max_y.saturating_sub(crop_y).saturating_add(1),
    )
    .to_image()
}

fn extract_alpha_mask(img: &RgbaImage) -> RgbaImage {
    let (width, height) = img.dimensions();
    let mut alpha = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        for x in 0..width {
            let a = img.get_pixel(x, y)[3];
            alpha.put_pixel(x, y, Rgba([a, a, a, 255]));
        }
    }
    alpha
}

fn line_mask_from_exact_colors(img: &RgbaImage) -> RgbaImage {
    let (width, height) = img.dimensions();
    let mut mask = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        for x in 0..width {
            let pixel = img.get_pixel(x, y);
            if pixel[3] == 0 {
                continue;
            }
            match (pixel[0], pixel[1], pixel[2]) {
                (10, 10, 10) | (20, 20, 20) | (35, 35, 40) => {
                    mask.put_pixel(x, y, Rgba([255, 255, 255, 255]));
                }
                _ => {}
            }
        }
    }
    mask
}

fn subtract_masks(a: &RgbaImage, b: &RgbaImage) -> RgbaImage {
    let (width, height) = a.dimensions();
    let mut out = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        for x in 0..width {
            let av = a.get_pixel(x, y)[0] as i16;
            let bv = b.get_pixel(x, y)[0] as i16;
            let value = av.saturating_sub(bv).clamp(0, 255) as u8;
            out.put_pixel(x, y, Rgba([value, value, value, 255]));
        }
    }
    out
}

fn create_vertical_ramp(width: u32, height: u32) -> RgbaImage {
    let mut ramp = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        let t = y as f32 / height.max(1) as f32;
        let value = (105.0 + 70.0 * (1.0 - (t - 0.55).abs() * 1.55)).clamp(0.0, 255.0) as u8;
        for x in 0..width {
            ramp.put_pixel(x, y, Rgba([value, value, value, 255]));
        }
    }
    ramp
}

fn multiply_masks(a: &RgbaImage, b: &RgbaImage) -> RgbaImage {
    let (width, height) = a.dimensions();
    let mut out = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        for x in 0..width {
            let av = a.get_pixel(x, y)[0] as f32 / 255.0;
            let bv = b.get_pixel(x, y)[0] as f32 / 255.0;
            let value = (av * bv * 255.0) as u8;
            out.put_pixel(x, y, Rgba([value, value, value, 255]));
        }
    }
    out
}

fn gaussian_blur_alpha(img: &RgbaImage, radius: f32) -> RgbaImage {
    let (width, height) = img.dimensions();
    let r = radius.ceil() as i32;
    let mut horizontal = img.clone();

    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let mut sum = 0.0;
            let mut total = 0.0;
            for dx in -r..=r {
                let nx = x + dx;
                if nx < 0 || nx >= width as i32 {
                    continue;
                }
                let weight = gaussian_weight(dx as f32, radius);
                sum += img.get_pixel(nx as u32, y as u32)[0] as f32 * weight;
                total += weight;
            }
            let value = if total > 0.0 { (sum / total).round() as u8 } else { 0 };
            horizontal.put_pixel(x as u32, y as u32, Rgba([value, value, value, 255]));
        }
    }

    let mut vertical = horizontal.clone();
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let mut sum = 0.0;
            let mut total = 0.0;
            for dy in -r..=r {
                let ny = y + dy;
                if ny < 0 || ny >= height as i32 {
                    continue;
                }
                let weight = gaussian_weight(dy as f32, radius);
                sum += horizontal.get_pixel(x as u32, ny as u32)[0] as f32 * weight;
                total += weight;
            }
            let value = if total > 0.0 { (sum / total).round() as u8 } else { 0 };
            vertical.put_pixel(x as u32, y as u32, Rgba([value, value, value, 255]));
        }
    }
    vertical
}

fn gaussian_weight(distance: f32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 1.0;
    }
    let sigma = radius.max(0.5) / 2.0;
    (-distance * distance / (2.0 * sigma * sigma)).exp()
}

fn set_alpha_channel(img: &mut RgbaImage, alpha: &RgbaImage) {
    let (width, height) = img.dimensions();
    for y in 0..height {
        for x in 0..width {
            let mut pixel = *img.get_pixel(x, y);
            pixel[3] = alpha.get_pixel(x, y)[0];
            img.put_pixel(x, y, pixel);
        }
    }
}

fn scale_alpha(mask: &RgbaImage, multiplier: f32) -> RgbaImage {
    let (width, height) = mask.dimensions();
    let mut scaled = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for y in 0..height {
        for x in 0..width {
            let value = (mask.get_pixel(x, y)[0] as f32 * multiplier).clamp(0.0, 255.0) as u8;
            scaled.put_pixel(x, y, Rgba([value, value, value, 255]));
        }
    }
    scaled
}

fn build_widget_mascot_from_frames(
    frames: &[RgbaImage],
    sequence: Vec<WidgetMascotSequenceStep>,
    spirit_hero_background: String,
) -> WidgetMascot {
    let quantized_frames = quantize_widget_frames(frames, WIDGET_MASCOT_ALPHABET.len() - 1);
    let (width, height) = quantized_frames
        .first()
        .map(RgbaImage::dimensions)
        .unwrap_or((0, 0));
    let palette = build_widget_palette(&quantized_frames);
    let frames = quantized_frames
        .iter()
        .map(|frame| build_widget_frame_string(frame, &palette))
        .collect();
    WidgetMascot {
        width,
        height,
        frame_ms: MASCOT_FRAME_MS,
        spirit_hero_background,
        palette: palette
            .into_iter()
            .map(|rgba| rgba_hex_raw(rgba[0], rgba[1], rgba[2], rgba[3]))
            .collect(),
        frames,
        sequence,
    }
}

fn quantize_widget_frames(frames: &[RgbaImage], max_colors: usize) -> Vec<RgbaImage> {
    let mut color_counts: HashMap<[u8; 4], u32> = HashMap::new();
    for frame in frames {
        for pixel in frame.pixels() {
            if pixel[3] == 0 {
                continue;
            }
            let rgba = [pixel[0], pixel[1], pixel[2], pixel[3]];
            *color_counts.entry(rgba).or_insert(0) += 1;
        }
    }

    if color_counts.len() <= max_colors {
        return frames.to_vec();
    }

    let mut palette: Vec<([u8; 4], u32)> = color_counts.into_iter().collect();
    palette.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let palette: Vec<[u8; 4]> = palette
        .into_iter()
        .take(max_colors)
        .map(|(rgba, _)| rgba)
        .collect();

    frames
        .iter()
        .map(|frame| quantize_widget_frame(frame, &palette))
        .collect()
}

fn quantize_widget_frame(frame: &RgbaImage, palette: &[[u8; 4]]) -> RgbaImage {
    let (width, height) = frame.dimensions();
    let mut quantized = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
    for (x, y, pixel) in frame.enumerate_pixels() {
        if pixel[3] == 0 {
            continue;
        }
        let rgba = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let nearest = nearest_widget_color(rgba, palette);
        quantized.put_pixel(x, y, Rgba(nearest));
    }
    quantized
}

fn nearest_widget_color(rgba: [u8; 4], palette: &[[u8; 4]]) -> [u8; 4] {
    let mut best = palette[0];
    let mut best_distance = color_distance(rgba, best);
    for &candidate in palette.iter().skip(1) {
        let distance = color_distance(rgba, candidate);
        if distance < best_distance {
            best = candidate;
            best_distance = distance;
        }
    }
    best
}

fn color_distance(left: [u8; 4], right: [u8; 4]) -> u32 {
    let dr = left[0] as i32 - right[0] as i32;
    let dg = left[1] as i32 - right[1] as i32;
    let db = left[2] as i32 - right[2] as i32;
    let da = left[3] as i32 - right[3] as i32;
    (dr * dr + dg * dg + db * db + da * da) as u32
}

fn build_widget_palette(frames: &[RgbaImage]) -> Vec<[u8; 4]> {
    let mut palette = Vec::new();
    for frame in frames {
        for pixel in frame.pixels() {
            if pixel[3] == 0 {
                continue;
            }
            let rgba = [pixel[0], pixel[1], pixel[2], pixel[3]];
            if !palette.iter().any(|entry| *entry == rgba) {
                palette.push(rgba);
            }
        }
    }
    assert!(
        palette.len() < WIDGET_MASCOT_ALPHABET.len(),
        "mascot palette exceeds widget alphabet"
    );
    palette
}

fn build_widget_frame_string(frame: &RgbaImage, palette: &[[u8; 4]]) -> String {
    let mut encoded = String::with_capacity((frame.width() * frame.height()) as usize);
    for pixel in frame.pixels() {
        if pixel[3] == 0 {
            encoded.push('.');
            continue;
        }
        let rgba = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let palette_index = palette
            .iter()
            .position(|entry| *entry == rgba)
            .expect("mascot pixel missing from palette");
        let symbol = WIDGET_MASCOT_ALPHABET
            .as_bytes()
            .get(palette_index + 1)
            .copied()
            .expect("mascot palette index missing from alphabet");
        encoded.push(symbol as char);
    }
    encoded
}

fn build_tui_frame(frame: &RgbaImage) -> TuiMascotFrame {
    let (width, height) = frame.dimensions();
    let mut rows = Vec::new();
    let mut y = 0;
    while y < height {
        let mut row = Vec::new();
        for x in 0..width {
            let top = *frame.get_pixel(x, y);
            let bottom = if y + 1 < height {
                *frame.get_pixel(x, y + 1)
            } else {
                Rgba([0, 0, 0, 0])
            };
            row.push(build_tui_cell(top, bottom));
        }
        rows.push(row);
        y += 2;
    }

    TuiMascotFrame { rows }
}

fn build_tui_cell(top: image::Rgba<u8>, bottom: image::Rgba<u8>) -> TuiMascotCell {
    let top_alpha = top[3] > 0;
    let bottom_alpha = bottom[3] > 0;

    match (top_alpha, bottom_alpha) {
        (false, false) => TuiMascotCell {
            glyph: ' ',
            fg: None,
            bg: None,
        },
        (true, false) => TuiMascotCell {
            glyph: '▀',
            fg: Some((top[0], top[1], top[2])),
            bg: None,
        },
        (false, true) => TuiMascotCell {
            glyph: '▄',
            fg: Some((bottom[0], bottom[1], bottom[2])),
            bg: None,
        },
        (true, true) => {
            let top_rgb = (top[0], top[1], top[2]);
            let bottom_rgb = (bottom[0], bottom[1], bottom[2]);
            if top_rgb == bottom_rgb {
                TuiMascotCell {
                    glyph: '█',
                    fg: Some(top_rgb),
                    bg: None,
                }
            } else {
                TuiMascotCell {
                    glyph: '▀',
                    fg: Some(top_rgb),
                    bg: Some(bottom_rgb),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_widget_mascot,
        mascot_headwear_preference,
        mascot_use_spirit,
        openness_value,
        stable_workspace_seed,
        WIDGET_MASCOT_ALPHABET,
    };
    use crate::binagotchy_gen;

    #[test]
    fn widget_mascot_palette_fits_alphabet() {
        let mascot = build_widget_mascot("/tmp/catdesk-spirit-test");
        assert!(mascot.palette.len() < WIDGET_MASCOT_ALPHABET.len());
    }

    #[test]
    fn widget_mascot_embeds_spirit_hero_background() {
        let spirit_workspace = (0..10_000)
            .map(|idx| format!("/tmp/catdesk-spirit-test-{idx}"))
            .find(|path| mascot_use_spirit(stable_workspace_seed(path)))
            .expect("expected a workspace path that triggers spirit");
        let mascot = build_widget_mascot(&spirit_workspace);
        assert!(mascot.spirit_hero_background.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn widget_mascot_keeps_normal_background_when_spirit_not_selected() {
        let normal_workspace = (0..10_000)
            .map(|idx| format!("/tmp/catdesk-normal-test-{idx}"))
            .find(|path| !mascot_use_spirit(stable_workspace_seed(path)))
            .expect("expected a workspace path that does not trigger spirit");
        let mascot = build_widget_mascot(&normal_workspace);
        assert!(mascot.spirit_hero_background.is_empty());
    }

    #[test]
    fn mascot_non_spirit_can_generate_headwear() {
        let workspace = (0..10_000)
            .map(|idx| format!("/tmp/catdesk-headwear-test-{idx}"))
            .find(|path| {
                let seed = stable_workspace_seed(path);
                if mascot_use_spirit(seed) {
                    return false;
                }
                let (_, traits) = binagotchy_gen::create_character(
                    Some(seed),
                    super::MASCOT_CANVAS,
                    super::MASCOT_UPSCALE,
                    "normal",
                    mascot_headwear_preference(false),
                    0.0,
                    openness_value(10),
                    1,
                );
                traits.get("headwear").is_some_and(|value| value != "none")
            })
            .expect("expected a non-spirit workspace path that generates headwear");

        let seed = stable_workspace_seed(&workspace);
        let (_, traits) = binagotchy_gen::create_character(
            Some(seed),
            super::MASCOT_CANVAS,
            super::MASCOT_UPSCALE,
            "normal",
            mascot_headwear_preference(false),
            0.0,
            openness_value(10),
            1,
        );
        assert_ne!(traits.get("headwear").map(String::as_str), Some("none"));
    }
}

fn rgba_hex_raw(r: u8, g: u8, b: u8, a: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

fn openness_value(value: u8) -> f32 {
    match value {
        10 => 1.0,
        5 => 0.5,
        0 => 0.0,
        _ => panic!("unsupported mascot eye openness"),
    }
}

use image::{Rgba, RgbaImage};
use ratatui::{
    prelude::{Color, Style},
    text::{Line, Span},
};
use serde::Serialize;

use crate::binagotchy_gen;

const MASCOT_CANVAS: u32 = 32;
const MASCOT_UPSCALE: u32 = 1;
const MASCOT_FRAME_MS: u64 = 50;
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
    let (frames, sequence) = mascot_widget_source(workspace_root);
    let cropped = crop_frames(&frames);
    build_widget_mascot_from_frames(&cropped, sequence)
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
    MASCOT_SEQUENCE
        .iter()
        .flat_map(|&(eye_openness, tail_state, repeat)| {
            let (frame, _) = binagotchy_gen::create_character(
                Some(seed),
                MASCOT_CANVAS,
                MASCOT_UPSCALE,
                0,
                "normal",
                "none",
                openness_value(eye_openness),
                tail_state,
            );
            std::iter::repeat_n(frame, repeat as usize)
        })
        .collect()
}

fn mascot_widget_source(workspace_root: &str) -> (Vec<RgbaImage>, Vec<WidgetMascotSequenceStep>) {
    let seed = stable_workspace_seed(workspace_root);
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
            binagotchy_gen::create_character(
                Some(seed),
                MASCOT_CANVAS,
                MASCOT_UPSCALE,
                0,
                "normal",
                "none",
                openness_value(eye_openness),
                tail_state,
            )
            .0
        })
        .collect();

    (frames, sequence)
}

fn stable_workspace_seed(workspace_root: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in workspace_root.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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

fn build_widget_mascot_from_frames(
    frames: &[RgbaImage],
    sequence: Vec<WidgetMascotSequenceStep>,
) -> WidgetMascot {
    let (width, height) = frames.first().map(RgbaImage::dimensions).unwrap_or((0, 0));
    let palette = build_widget_palette(frames);
    let frames = frames
        .iter()
        .map(|frame| build_widget_frame_string(frame, &palette))
        .collect();
    WidgetMascot {
        width,
        height,
        frame_ms: MASCOT_FRAME_MS,
        palette: palette
            .into_iter()
            .map(|rgba| rgba_hex_raw(rgba[0], rgba[1], rgba[2], rgba[3]))
            .collect(),
        frames,
        sequence,
    }
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

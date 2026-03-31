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
const MASCOT_SEQUENCE: &[(f32, i32, u8)] = &[
    (1.0, 1, 7),
    (1.0, 0, 7),
    (1.0, 1, 7),
    (1.0, 0, 7),
    (1.0, 1, 2),
    (0.5, 1, 1),
    (0.0, 1, 4),
    (0.5, 0, 1),
    (1.0, 0, 6),
    (1.0, 1, 7),
    (1.0, 0, 7),
];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetMascotRun {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub color: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetMascotFrame {
    pub runs: Vec<WidgetMascotRun>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetMascot {
    pub width: u32,
    pub height: u32,
    pub frame_ms: u64,
    pub frames: Vec<WidgetMascotFrame>,
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
    let frames = mascot_source_frames(workspace_root);
    let cropped = crop_frames(&frames);
    build_widget_mascot_from_frames(&cropped)
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
                eye_openness,
                tail_state,
            );
            std::iter::repeat_n(frame, repeat as usize)
        })
        .collect()
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

fn build_widget_mascot_from_frames(frames: &[RgbaImage]) -> WidgetMascot {
    let (width, height) = frames.first().map(RgbaImage::dimensions).unwrap_or((0, 0));
    let frames = frames.iter().map(build_widget_frame).collect();
    WidgetMascot {
        width,
        height,
        frame_ms: MASCOT_FRAME_MS,
        frames,
    }
}

fn build_widget_frame(frame: &RgbaImage) -> WidgetMascotFrame {
    let (width, height) = frame.dimensions();
    let mut runs = Vec::new();

    for y in 0..height {
        let mut x = 0;
        while x < width {
            let pixel = frame.get_pixel(x, y);
            if pixel[3] == 0 {
                x += 1;
                continue;
            }
            let color = rgba_hex(pixel);
            let mut run_width = 1_u32;
            while x + run_width < width {
                let next = frame.get_pixel(x + run_width, y);
                if next[3] == 0 || rgba_hex(next) != color {
                    break;
                }
                run_width += 1;
            }
            runs.push(WidgetMascotRun {
                x,
                y,
                width: run_width,
                color,
            });
            x += run_width;
        }
    }

    WidgetMascotFrame { runs }
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

fn rgba_hex(pixel: &Rgba<u8>) -> String {
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        pixel[0], pixel[1], pixel[2], pixel[3]
    )
}

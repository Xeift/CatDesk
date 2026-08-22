use crate::{
    mascot::{MascotPack, TuiMascotFrame, render_tui_lines},
    theme,
};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    prelude::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use std::{collections::VecDeque, io::Stdout, time::Duration};

const STARTUP_DURATION: Duration = Duration::from_millis(1_850);
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const TRACE_DURATION_MS: u128 = 720;
const TRACE_FLASH_MS: u128 = 140;
const FILL_START_MS: u128 = 820;
const FILL_DURATION_MS: u128 = 320;
const MASCOT_ANIMATION_START_MS: u128 = 1_160;
const WORDMARK_APPEAR_MS: u128 = 260;
const VERSION_APPEAR_MS: u128 = 700;
const TRACE_LEAD_PIXELS: usize = 9;
const LARGE_LAYOUT_MIN_WIDTH: u16 = 82;
const LARGE_LAYOUT_MIN_HEIGHT: u16 = 17;

const CATDESK_WORDMARK: [&str; 3] = [
    "█▀▀ █▀█ ▀█▀ █▀▄ █▀▀ █▀▀ █▄▀",
    "█   █▀█  █  █ █ █▀  ▀▀█ █▀▄",
    "▀▀▀ ▀ ▀  ▀  ▀▀  ▀▀▀ ▀▀▀ ▀ ▀",
];

pub async fn run_startup_intro(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    theme: &theme::ThemeDef,
    mascot: &MascotPack,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    loop {
        let elapsed = started.elapsed();
        terminal.draw(|frame| draw_startup_frame(frame, theme, mascot, elapsed))?;

        if elapsed >= STARTUP_DURATION {
            return Ok(());
        }

        if event::poll(FRAME_INTERVAL)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => return Ok(()),
                _ => {}
            }
        }
    }
}

pub fn draw_startup_frame(
    frame: &mut Frame,
    theme: &theme::ThemeDef,
    mascot: &MascotPack,
    elapsed: Duration,
) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let palette = theme.palette;
    let elapsed_ms = elapsed.as_millis();
    let Some(initial_frame) = mascot.tui_frames.first() else {
        draw_wordmark_only(frame, area, &palette, elapsed_ms);
        draw_skip_hint(frame, area, &palette);
        return;
    };

    let mascot_width = initial_frame
        .rows
        .iter()
        .map(|row| row.len())
        .max()
        .unwrap_or(0) as u16;
    let mascot_height = initial_frame.rows.len() as u16;

    if area.width >= LARGE_LAYOUT_MIN_WIDTH && area.height >= LARGE_LAYOUT_MIN_HEIGHT {
        draw_horizontal_lockup(
            frame,
            area,
            &palette,
            mascot,
            initial_frame,
            mascot_width,
            mascot_height,
            elapsed_ms,
        );
    } else {
        draw_compact_lockup(
            frame,
            area,
            &palette,
            mascot,
            initial_frame,
            mascot_width,
            mascot_height,
            elapsed_ms,
        );
    }

    draw_skip_hint(frame, area, &palette);
}

fn draw_horizontal_lockup(
    frame: &mut Frame,
    area: Rect,
    palette: &theme::Palette,
    mascot: &MascotPack,
    initial_frame: &TuiMascotFrame,
    mascot_width: u16,
    mascot_height: u16,
    elapsed_ms: u128,
) {
    let version = version_label();
    let wordmark_width = big_wordmark_width().max(version.chars().count() as u16);
    let gap = 5_u16;
    let content_width = mascot_width
        .saturating_add(gap)
        .saturating_add(wordmark_width)
        .min(area.width);
    let content_height = mascot_height.max(5).min(area.height);
    let content = centered_area(area, content_width, content_height);

    let mascot_area = Rect::new(
        content.x,
        content
            .y
            .saturating_add(content.height.saturating_sub(mascot_height) / 2),
        mascot_width.min(content.width),
        mascot_height.min(content.height),
    );
    draw_mascot_reveal(
        frame,
        mascot_area,
        mascot,
        initial_frame,
        palette,
        elapsed_ms,
    );

    let wordmark_x = mascot_area
        .x
        .saturating_add(mascot_width)
        .saturating_add(gap);
    let wordmark_area = Rect::new(
        wordmark_x,
        content
            .y
            .saturating_add(content.height.saturating_sub(5) / 2),
        content.right().saturating_sub(wordmark_x),
        5,
    );
    draw_wordmark(frame, wordmark_area, palette, elapsed_ms, Alignment::Left);
}

fn draw_compact_lockup(
    frame: &mut Frame,
    area: Rect,
    palette: &theme::Palette,
    mascot: &MascotPack,
    initial_frame: &TuiMascotFrame,
    mascot_width: u16,
    mascot_height: u16,
    elapsed_ms: u128,
) {
    let version = version_label();
    let text_width = big_wordmark_width().max(version.chars().count() as u16);
    let layout_area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
    let can_show_mascot = mascot_width <= layout_area.width.saturating_sub(2)
        && mascot_height.saturating_add(5) <= layout_area.height;

    if !can_show_mascot {
        draw_wordmark_only(frame, layout_area, palette, elapsed_ms);
        return;
    }

    let content_width = mascot_width.max(text_width).min(layout_area.width);
    let content_height = mascot_height.saturating_add(5).min(layout_area.height);
    let content = centered_area(layout_area, content_width, content_height);
    let mascot_area = Rect::new(
        content
            .x
            .saturating_add(content.width.saturating_sub(mascot_width) / 2),
        content.y,
        mascot_width.min(content.width),
        mascot_height.min(content.height),
    );
    draw_mascot_reveal(
        frame,
        mascot_area,
        mascot,
        initial_frame,
        palette,
        elapsed_ms,
    );

    let wordmark_y = mascot_area.y.saturating_add(mascot_height);
    if wordmark_y < content.bottom() {
        draw_wordmark(
            frame,
            Rect::new(
                content.x,
                wordmark_y,
                content.width,
                5.min(content.bottom() - wordmark_y),
            ),
            palette,
            elapsed_ms,
            Alignment::Center,
        );
    }
}

fn draw_wordmark_only(frame: &mut Frame, area: Rect, palette: &theme::Palette, elapsed_ms: u128) {
    let height = if area.height >= 5 { 5 } else { area.height };
    let compact = centered_area(
        area,
        area.width.saturating_sub(2).min(big_wordmark_width()),
        height,
    );
    draw_wordmark(frame, compact, palette, elapsed_ms, Alignment::Center);
}

fn draw_wordmark(
    frame: &mut Frame,
    area: Rect,
    palette: &theme::Palette,
    elapsed_ms: u128,
    alignment: Alignment,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    if area.height < 3 || area.width < big_wordmark_width() {
        let title = if elapsed_ms >= WORDMARK_APPEAR_MS {
            "CATDESK"
        } else {
            ""
        };
        let version = if elapsed_ms >= VERSION_APPEAR_MS {
            version_label()
        } else {
            String::new()
        };
        let mut lines = vec![Line::from(Span::styled(
            title,
            Style::default()
                .fg(palette.title_fg)
                .add_modifier(Modifier::BOLD),
        ))];
        if area.height >= 2 {
            lines.push(Line::from(Span::styled(
                version,
                Style::default().fg(palette.muted_fg),
            )));
        }
        frame.render_widget(Paragraph::new(lines).alignment(alignment), area);
        return;
    }

    let width = big_wordmark_width() as usize;
    let progress = ((elapsed_ms.saturating_sub(WORDMARK_APPEAR_MS)) as f32 / 430.0).clamp(0.0, 1.0);
    let reveal = trace_ease(progress);
    let visible_columns = ((width as f32 * reveal).ceil() as usize).min(width);
    let lead = visible_columns.saturating_sub(1);

    let mut lines = CATDESK_WORDMARK
        .iter()
        .map(|row| {
            let spans = row
                .chars()
                .enumerate()
                .map(|(column, ch)| {
                    if column >= visible_columns {
                        return Span::raw(" ");
                    }
                    let style = if progress < 1.0 && column.abs_diff(lead) <= 1 {
                        Style::default()
                            .fg(palette.info_fg)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(palette.title_fg)
                            .add_modifier(Modifier::BOLD)
                    };
                    Span::styled(ch.to_string(), style)
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect::<Vec<_>>();

    if area.height >= 4 {
        lines.push(Line::from(""));
    }
    if area.height >= 5 {
        let version = if elapsed_ms >= VERSION_APPEAR_MS {
            version_label()
        } else {
            String::new()
        };
        lines.push(Line::from(Span::styled(
            version,
            Style::default().fg(palette.muted_fg),
        )));
    }

    frame.render_widget(Paragraph::new(lines).alignment(alignment), area);
}

fn big_wordmark_width() -> u16 {
    CATDESK_WORDMARK
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as u16
}

fn draw_mascot_reveal(
    frame: &mut Frame,
    area: Rect,
    mascot: &MascotPack,
    initial_frame: &TuiMascotFrame,
    palette: &theme::Palette,
    elapsed_ms: u128,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if elapsed_ms >= MASCOT_ANIMATION_START_MS {
        let animation_ms = elapsed_ms.saturating_sub(MASCOT_ANIMATION_START_MS);
        let animated_frame = mascot.current_tui_frame(animation_ms);
        frame.render_widget(
            Paragraph::new(render_tui_lines(animated_frame, area.height)),
            area,
        );
        return;
    }

    let (pixel_width, pixel_height, order) = mascot_outline_order(initial_frame);
    if pixel_width == 0 || pixel_height == 0 || order.is_empty() {
        return;
    }

    let pixel_colors = mascot_pixel_colors(initial_frame);
    let occupancy = pixel_colors
        .iter()
        .map(|row| row.iter().map(Option::is_some).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let (fill_depths, max_fill_depth) = mascot_fill_depths(&occupancy);

    let trace_progress = (elapsed_ms as f32 / TRACE_DURATION_MS as f32).clamp(0.0, 1.0);
    let visible_count =
        ((order.len() as f32 * trace_ease(trace_progress)).ceil() as usize).min(order.len());
    let settled = visible_count >= order.len();
    let mut order_map = vec![vec![None; pixel_width]; pixel_height];
    for (index, &(x, y)) in order.iter().enumerate() {
        order_map[y][x] = Some(index);
    }

    let flash =
        if elapsed_ms >= TRACE_DURATION_MS && elapsed_ms < TRACE_DURATION_MS + TRACE_FLASH_MS {
            let flash_progress = (elapsed_ms - TRACE_DURATION_MS) as f32 / TRACE_FLASH_MS as f32;
            (std::f32::consts::PI * flash_progress).sin()
        } else {
            0.0
        };
    let fill_started = elapsed_ms >= FILL_START_MS;
    let fill_progress = ((elapsed_ms.saturating_sub(FILL_START_MS)) as f32
        / FILL_DURATION_MS as f32)
        .clamp(0.0, 1.0);
    let fill_limit = if fill_started {
        ((max_fill_depth as f32 * ease_out_cubic(fill_progress)).ceil() as usize)
            .min(max_fill_depth)
    } else {
        0
    };

    let render_rows = area.height.min(initial_frame.rows.len() as u16) as usize;
    let render_width = area.width.min(pixel_width as u16) as usize;
    let mut lines = Vec::with_capacity(render_rows);

    for row_index in 0..render_rows {
        let top_y = row_index * 2;
        let bottom_y = top_y + 1;
        let mut spans = Vec::with_capacity(render_width);

        for x in 0..render_width {
            let top = reveal_pixel_color(
                pixel_colors
                    .get(top_y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten(),
                order_map
                    .get(top_y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten(),
                fill_depths
                    .get(top_y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten(),
                visible_count,
                settled,
                flash,
                fill_started,
                fill_limit,
                palette,
            );
            let bottom = reveal_pixel_color(
                pixel_colors
                    .get(bottom_y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten(),
                order_map
                    .get(bottom_y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten(),
                fill_depths
                    .get(bottom_y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten(),
                visible_count,
                settled,
                flash,
                fill_started,
                fill_limit,
                palette,
            );
            spans.push(trace_cell_span(top, bottom));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
}

#[allow(clippy::too_many_arguments)]
fn reveal_pixel_color(
    actual_rgb: Option<(u8, u8, u8)>,
    order_index: Option<usize>,
    fill_depth: Option<usize>,
    visible_count: usize,
    settled: bool,
    flash: f32,
    fill_started: bool,
    fill_limit: usize,
    palette: &theme::Palette,
) -> Option<Color> {
    if settled && flash > 0.32 && order_index.is_some() {
        return Some(palette.info_fg);
    }

    if fill_started && fill_depth.is_some_and(|depth| depth <= fill_limit) {
        if let Some((r, g, b)) = actual_rgb {
            return Some(Color::Rgb(r, g, b));
        }
    }

    let index = order_index?;
    if index >= visible_count {
        return None;
    }

    if settled {
        return Some(palette.title_fg);
    }

    if visible_count.saturating_sub(index) <= TRACE_LEAD_PIXELS {
        Some(palette.info_fg)
    } else {
        Some(palette.secondary_fg)
    }
}

fn trace_cell_span(top: Option<Color>, bottom: Option<Color>) -> Span<'static> {
    match (top, bottom) {
        (None, None) => Span::raw(" "),
        (Some(color), None) => {
            Span::styled("▀", Style::default().fg(color).add_modifier(Modifier::BOLD))
        }
        (None, Some(color)) => {
            Span::styled("▄", Style::default().fg(color).add_modifier(Modifier::BOLD))
        }
        (Some(top_color), Some(bottom_color)) if top_color == bottom_color => Span::styled(
            "█",
            Style::default().fg(top_color).add_modifier(Modifier::BOLD),
        ),
        (Some(top_color), Some(bottom_color)) => Span::styled(
            "▀",
            Style::default()
                .fg(top_color)
                .bg(bottom_color)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

fn mascot_outline_order(frame: &TuiMascotFrame) -> (usize, usize, Vec<(usize, usize)>) {
    let occupancy = mascot_occupancy(frame);
    let height = occupancy.len();
    let width = occupancy.first().map_or(0, Vec::len);
    if width == 0 || height == 0 {
        return (width, height, Vec::new());
    }

    let mut boundary = vec![vec![false; width]; height];
    let mut boundary_count = 0;
    for y in 0..height {
        for x in 0..width {
            if !occupancy[y][x] {
                continue;
            }
            let exposed = x == 0
                || y == 0
                || x + 1 == width
                || y + 1 == height
                || !occupancy[y][x - 1]
                || !occupancy[y][x + 1]
                || !occupancy[y - 1][x]
                || !occupancy[y + 1][x];
            if exposed {
                boundary[y][x] = true;
                boundary_count += 1;
            }
        }
    }

    if boundary_count == 0 {
        return (width, height, Vec::new());
    }

    let center_x = width / 2;
    let start = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| boundary[y][x])
        .min_by_key(|&(x, y)| (y, x.abs_diff(center_x), x))
        .expect("non-empty boundary must have a start pixel");

    let mut visited = vec![vec![false; width]; height];
    let mut order = Vec::with_capacity(boundary_count);
    let mut current = start;

    while order.len() < boundary_count {
        let (x, y) = current;
        if !visited[y][x] {
            visited[y][x] = true;
            order.push(current);
        }

        if let Some(next) = adjacent_unvisited_boundary(current, &boundary, &visited) {
            current = next;
            continue;
        }

        let mut nearest = None;
        let mut nearest_distance = usize::MAX;
        for next_y in 0..height {
            for next_x in 0..width {
                if !boundary[next_y][next_x] || visited[next_y][next_x] {
                    continue;
                }
                let dx = x.abs_diff(next_x);
                let dy = y.abs_diff(next_y);
                let distance = dx * dx + dy * dy;
                if distance < nearest_distance {
                    nearest = Some((next_x, next_y));
                    nearest_distance = distance;
                }
            }
        }

        let Some(next) = nearest else {
            break;
        };
        current = next;
    }

    (width, height, order)
}

fn adjacent_unvisited_boundary(
    current: (usize, usize),
    boundary: &[Vec<bool>],
    visited: &[Vec<bool>],
) -> Option<(usize, usize)> {
    const OFFSETS: [(isize, isize); 8] = [
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
        (-1, 0),
        (-1, -1),
        (0, -1),
        (1, -1),
    ];
    let width = boundary.first().map_or(0, Vec::len) as isize;
    let height = boundary.len() as isize;
    let (x, y) = (current.0 as isize, current.1 as isize);

    OFFSETS.into_iter().find_map(|(dx, dy)| {
        let next_x = x + dx;
        let next_y = y + dy;
        if next_x < 0 || next_y < 0 || next_x >= width || next_y >= height {
            return None;
        }
        let next = (next_x as usize, next_y as usize);
        (boundary[next.1][next.0] && !visited[next.1][next.0]).then_some(next)
    })
}

fn mascot_occupancy(frame: &TuiMascotFrame) -> Vec<Vec<bool>> {
    let width = frame.rows.iter().map(|row| row.len()).max().unwrap_or(0);
    let height = frame.rows.len() * 2;
    let mut occupancy = vec![vec![false; width]; height];

    for (row_index, row) in frame.rows.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            let top_y = row_index * 2;
            let bottom_y = top_y + 1;
            match cell.glyph {
                '▀' => {
                    occupancy[top_y][x] = cell.fg.is_some();
                    occupancy[bottom_y][x] = cell.bg.is_some();
                }
                '▄' => occupancy[bottom_y][x] = cell.fg.is_some(),
                '█' => {
                    occupancy[top_y][x] = cell.fg.is_some();
                    occupancy[bottom_y][x] = cell.fg.is_some();
                }
                ' ' => {
                    occupancy[top_y][x] = cell.bg.is_some();
                    occupancy[bottom_y][x] = cell.bg.is_some();
                }
                _ => {
                    let occupied = cell.fg.is_some() || cell.bg.is_some();
                    occupancy[top_y][x] = occupied;
                    occupancy[bottom_y][x] = occupied;
                }
            }
        }
    }

    occupancy
}

fn mascot_pixel_colors(frame: &TuiMascotFrame) -> Vec<Vec<Option<(u8, u8, u8)>>> {
    let width = frame.rows.iter().map(|row| row.len()).max().unwrap_or(0);
    let height = frame.rows.len() * 2;
    let mut pixels = vec![vec![None; width]; height];

    for (row_index, row) in frame.rows.iter().enumerate() {
        let top_y = row_index * 2;
        let bottom_y = top_y + 1;
        for (x, cell) in row.iter().enumerate() {
            match cell.glyph {
                '▀' => {
                    pixels[top_y][x] = cell.fg;
                    pixels[bottom_y][x] = cell.bg;
                }
                '▄' => pixels[bottom_y][x] = cell.fg,
                '█' => {
                    pixels[top_y][x] = cell.fg;
                    pixels[bottom_y][x] = cell.fg;
                }
                ' ' => {
                    pixels[top_y][x] = cell.bg;
                    pixels[bottom_y][x] = cell.bg;
                }
                _ => {
                    let color = cell.fg.or(cell.bg);
                    pixels[top_y][x] = color;
                    pixels[bottom_y][x] = color;
                }
            }
        }
    }

    pixels
}

fn mascot_fill_depths(occupancy: &[Vec<bool>]) -> (Vec<Vec<Option<usize>>>, usize) {
    let height = occupancy.len();
    let width = occupancy.first().map_or(0, Vec::len);
    let mut depths = vec![vec![None; width]; height];
    let mut queue = VecDeque::new();

    for y in 0..height {
        for x in 0..width {
            if !occupancy[y][x] {
                continue;
            }
            let exposed = x == 0
                || y == 0
                || x + 1 == width
                || y + 1 == height
                || !occupancy[y][x - 1]
                || !occupancy[y][x + 1]
                || !occupancy[y - 1][x]
                || !occupancy[y + 1][x];
            if exposed {
                depths[y][x] = Some(0);
                queue.push_back((x, y));
            }
        }
    }

    let mut max_depth = 0;
    const OFFSETS: [(isize, isize); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
    while let Some((x, y)) = queue.pop_front() {
        let depth = depths[y][x].unwrap_or(0);
        max_depth = max_depth.max(depth);
        for (dx, dy) in OFFSETS {
            let next_x = x as isize + dx;
            let next_y = y as isize + dy;
            if next_x < 0 || next_y < 0 || next_x >= width as isize || next_y >= height as isize {
                continue;
            }
            let next_x = next_x as usize;
            let next_y = next_y as usize;
            if !occupancy[next_y][next_x] || depths[next_y][next_x].is_some() {
                continue;
            }
            let next_depth = depth + 1;
            depths[next_y][next_x] = Some(next_depth);
            max_depth = max_depth.max(next_depth);
            queue.push_back((next_x, next_y));
        }
    }

    (depths, max_depth)
}

fn version_label() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn draw_skip_hint(frame: &mut Frame, area: Rect, palette: &theme::Palette) {
    if area.height < 2 {
        return;
    }
    frame.render_widget(
        Paragraph::new("press any key to skip")
            .style(Style::default().fg(palette.muted_fg))
            .alignment(Alignment::Center),
        Rect::new(area.x, area.bottom().saturating_sub(2), area.width, 1),
    );
}

fn ease_out_cubic(value: f32) -> f32 {
    1.0 - (1.0 - value).powi(3)
}

fn trace_ease(value: f32) -> f32 {
    value.clamp(0.0, 1.0).powf(1.55)
}

fn centered_area(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::{draw_startup_frame, mascot_outline_order};
    use crate::mascot;
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::Duration;

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn startup_screen_renders_binagotchy_outline_wordmark_and_version() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let theme = crate::theme::all()[0];
        let mascot = mascot::build_workspace_mascot(1);

        terminal
            .draw(|frame| draw_startup_frame(frame, &theme, &mascot, Duration::from_millis(1_000)))
            .expect("draw startup frame");

        let text = buffer_text(&terminal);
        assert!(text.contains("█▀▀ █▀█"));
        assert!(text.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("press any key to skip"));
        assert!(text.contains('▀') || text.contains('▄') || text.contains('█'));
        assert!(!text.contains("WORKSPACE LINK"));
    }

    #[test]
    fn startup_screen_uses_stacked_layout_in_small_terminal() {
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let theme = crate::theme::all()[0];
        let mascot = mascot::build_workspace_mascot(1);

        terminal
            .draw(|frame| draw_startup_frame(frame, &theme, &mascot, Duration::from_millis(1_000)))
            .expect("draw compact startup frame");

        let text = buffer_text(&terminal);
        assert!(text.contains("█▀▀ █▀█"));
        assert!(text.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains('▀') || text.contains('▄') || text.contains('█'));
    }

    #[test]
    fn startup_fill_restores_binagotchy_rgb_colors() {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        let theme = crate::theme::all()[0];
        let mascot = mascot::build_workspace_mascot(1);
        let expected_rgb = mascot
            .tui_frames
            .first()
            .and_then(|frame| {
                frame
                    .rows
                    .iter()
                    .flatten()
                    .find_map(|cell| cell.fg.or(cell.bg))
            })
            .expect("mascot rgb color");

        terminal
            .draw(|frame| draw_startup_frame(frame, &theme, &mascot, Duration::from_millis(1_150)))
            .expect("draw filled startup frame");

        let buffer = terminal.backend().buffer();
        let has_expected_rgb = (0..buffer.area.height).any(|row| {
            (0..buffer.area.width).any(|column| {
                let cell = &buffer[(column, row)];
                cell.fg
                    == ratatui::style::Color::Rgb(expected_rgb.0, expected_rgb.1, expected_rgb.2)
                    || cell.bg
                        == ratatui::style::Color::Rgb(
                            expected_rgb.0,
                            expected_rgb.1,
                            expected_rgb.2,
                        )
            })
        });

        assert!(has_expected_rgb);
    }

    #[test]
    fn startup_outline_is_smaller_than_filled_mascot() {
        let mascot = mascot::build_workspace_mascot(1);
        let frame = mascot.tui_frames.first().expect("mascot frame");
        let occupied_pixels = frame
            .rows
            .iter()
            .flat_map(|row| row.iter())
            .map(|cell| match cell.glyph {
                '▀' if cell.bg.is_some() => 2,
                '▀' | '▄' => 1,
                '█' => 2,
                _ => 0,
            })
            .sum::<usize>();
        let (_, _, outline) = mascot_outline_order(frame);

        assert!(!outline.is_empty());
        assert!(outline.len() < occupied_pixels);
    }
}

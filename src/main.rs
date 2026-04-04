mod binagotchy_gen;
mod browser;
mod command;
mod devtools;
mod mascot;
mod mcp;
mod ngrok;
mod server;
mod state;
mod theme;
mod workspace_tools;

use crossterm::{
    ExecutableCommand,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use devtools::DevtoolsBridge;
use mascot::render_tui_lines;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use state::{
    AppState, FLOW_ANIM_CELLS, FLOW_BOOTSTRAP_PHASES, FlowAnimKind, FlowAnimSegment,
    FlowDirection, Mode, SessionFlow, SharedState, ToolMode, UsageTotals, app_config_path,
    load_ngrok_authtoken, save_ngrok_authtoken,
};
use std::io::{Write, stdout};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::Mutex;

const FLOW_ROW_CELLS: usize = FLOW_ANIM_CELLS;
const REMOTE_CONNECT_UI_GRACE_MS: u128 = 8_000;
const UI_POLL_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);

// ── Selection ───────────────────────────────────────────────

struct Selection {
    start: Option<(u16, u16)>,
    end: Option<(u16, u16)>,
    dragging: bool,
}

impl Selection {
    fn new() -> Self {
        Self {
            start: None,
            end: None,
            dragging: false,
        }
    }
    fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.dragging = false;
    }
    fn range(&self) -> Option<((u16, u16), (u16, u16))> {
        match (self.start, self.end) {
            (Some(s), Some(e)) => {
                let (r0, c0, r1, c1) = if (s.1, s.0) <= (e.1, e.0) {
                    (s.1, s.0, e.1, e.0)
                } else {
                    (e.1, e.0, s.1, s.0)
                };
                Some(((c0, r0), (c1, r1)))
            }
            _ => None,
        }
    }
}

fn extract_from_screen(lines: &[String], start: (u16, u16), end: (u16, u16)) -> String {
    let (c0, r0) = start;
    let (c1, r1) = end;
    let mut result = String::new();
    for row in r0..=r1 {
        let idx = row as usize;
        if idx >= lines.len() {
            break;
        }
        let line: Vec<char> = lines[idx].chars().collect();
        let cs = if row == r0 { c0 as usize } else { 0 };
        let ce = if row == r1 {
            (c1 as usize).min(line.len().saturating_sub(1))
        } else {
            line.len().saturating_sub(1)
        };
        for col in cs..=ce {
            if col < line.len() {
                result.push(line[col]);
            }
        }
        if row != r1 {
            result.push('\n');
        }
    }
    result
        .lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn current_anim_segment(flow: &SessionFlow, now_millis: u128) -> Option<FlowAnimSegment> {
    if let Some(seg) = flow
        .anim_queue
        .iter()
        .find(|seg| seg.started_ms <= now_millis && now_millis < seg.ends_ms)
    {
        return Some(*seg);
    }
    flow.anim_queue.front().copied()
}

fn should_display_flow_row(flow: &SessionFlow, remote_connected: bool) -> bool {
    remote_connected || flow.closing_started_ms.is_some() || !flow.anim_queue.is_empty()
}

fn flow_direction(flow: Option<&SessionFlow>, now_millis: u128) -> FlowDirection {
    if let Some(flow) = flow {
        if let Some(seg) = current_anim_segment(flow, now_millis) {
            return seg.direction;
        }
        if let Some(seg) = flow.anim_queue.back() {
            return seg.direction;
        }
        if flow.closing_started_ms.is_some() {
            return flow.last_direction;
        }
    }
    FlowDirection::Forward
}

fn flow_lit_count(
    flow: Option<&SessionFlow>,
    active: bool,
    now_millis: u128,
    direction: FlowDirection,
    forward_delay_cells: u64,
    backward_delay_cells: u64,
    _close_delay_cells: u64,
    cells: usize,
) -> usize {
    if !active && flow.is_none() {
        return 0;
    }

    if let Some(flow) = flow {
        if flow.closing_started_ms.is_some() {
            // A closed SSE stream should render as fully unlit immediately.
            0
        } else if active {
            if let Some(seg) = current_anim_segment(flow, now_millis) {
                if seg.kind == FlowAnimKind::Turn {
                    0
                } else {
                    let step_ms = seg.step_ms.max(1) as u128;
                    let delay_cells = match direction {
                        FlowDirection::Forward => forward_delay_cells,
                        FlowDirection::Backward => backward_delay_cells,
                    };
                    let delay_ms = (delay_cells.saturating_mul(seg.step_ms.max(1))) as u128;
                    let elapsed_ms = now_millis.saturating_sub(seg.started_ms);
                    if elapsed_ms < delay_ms {
                        0
                    } else {
                        let progressed = elapsed_ms - delay_ms;
                        (((progressed / step_ms) as usize) + 1).min(cells)
                    }
                }
            } else {
                cells
            }
        } else {
            0
        }
    } else if active {
        cells
    } else {
        0
    }
}

fn debug_lane(direction: FlowDirection, lit_count: usize, cells: usize) -> String {
    let dashes = cells.saturating_sub(1);
    let mut out = String::with_capacity(cells);
    for i in 0..cells {
        match direction {
            FlowDirection::Forward => {
                if i == dashes {
                    out.push('>');
                } else if i < lit_count {
                    out.push('#');
                } else {
                    out.push('-');
                }
            }
            FlowDirection::Backward => {
                if i == 0 {
                    out.push('<');
                } else if i >= cells.saturating_sub(lit_count) {
                    out.push('#');
                } else {
                    out.push('-');
                }
            }
        }
    }
    out
}

fn flow_phase(flow: &SessionFlow, now_millis: u128) -> &'static str {
    if flow.closing_started_ms.is_some() {
        return "close";
    }
    if let Some(seg) = current_anim_segment(flow, now_millis) {
        return match seg.kind {
            FlowAnimKind::Turn => "turn",
            FlowAnimKind::Move => match seg.direction {
                FlowDirection::Forward => "request",
                FlowDirection::Backward => "response",
            },
        };
    }
    "idle"
}

fn latest_flow_action(flow: &SessionFlow) -> String {
    flow.events
        .iter()
        .rev()
        .find_map(|event| {
            if let Some(tool) = event.strip_prefix("tools/call:") {
                if tool.is_empty() {
                    None
                } else {
                    Some(tool.to_string())
                }
            } else if event.is_empty() {
                None
            } else {
                Some(event.clone())
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlowPhaseStepState {
    Future,
    Pending,
    Complete,
}

fn flow_phase_bounds(phase_index: usize) -> (usize, usize) {
    let start = FLOW_BOOTSTRAP_PHASES
        .iter()
        .take(phase_index)
        .map(|phase| phase.steps.len())
        .sum::<usize>();
    let end = start + FLOW_BOOTSTRAP_PHASES[phase_index].steps.len();
    (start, end)
}

fn flow_phase_step_state(flow: Option<&SessionFlow>, step_index: usize) -> FlowPhaseStepState {
    let Some(flow) = flow else {
        return FlowPhaseStepState::Future;
    };
    if step_index < flow.bootstrap_completed_steps {
        FlowPhaseStepState::Complete
    } else if flow.bootstrap_pending_steps.contains(&step_index) {
        FlowPhaseStepState::Pending
    } else {
        FlowPhaseStepState::Future
    }
}

fn flow_phase_status_label(flow: Option<&SessionFlow>, phase_index: usize) -> Option<String> {
    let Some(flow) = flow else {
        return None;
    };
    let (start, end) = flow_phase_bounds(phase_index);
    if flow.bootstrap_completed_steps >= end {
        return Some("✓".to_string());
    }
    if let Some(step_index) = flow
        .bootstrap_pending_steps
        .iter()
        .copied()
        .find(|step_index| (start..end).contains(step_index))
    {
        let step = &FLOW_BOOTSTRAP_PHASES[phase_index].steps[step_index - start];
        return Some(step.label.to_string());
    }
    if (start..end).contains(&flow.bootstrap_completed_steps.saturating_sub(1))
        && flow.bootstrap_completed_steps > start
    {
        let step_index = flow.bootstrap_completed_steps - 1;
        let step = &FLOW_BOOTSTRAP_PHASES[phase_index].steps[step_index - start];
        return Some(step.label.to_string());
    }
    None
}

fn flow_phase_lines(
    flow: Option<&SessionFlow>,
    palette: &theme::Palette,
    status_style: Style,
) -> Vec<Line<'static>> {
    const TITLE_STATUS_GAP: usize = 4;
    const STATUS_ANIM_GAP: usize = 4;
    let title_width = FLOW_BOOTSTRAP_PHASES
        .iter()
        .enumerate()
        .map(|(phase_index, phase)| format!("    Phase {}: {}", phase_index + 1, phase.title))
        .map(|title| title.chars().count())
        .max()
        .unwrap_or(0);
    let status_width = FLOW_BOOTSTRAP_PHASES
        .iter()
        .flat_map(|phase| {
            std::iter::once("✓".to_string())
                .chain(phase.steps.iter().map(|step| step.label.to_string()))
                .map(|status| format!("[{status}]").chars().count())
        })
        .max()
        .unwrap_or(0);
    let pending_style = Style::default()
        .fg(palette.info_fg)
        .add_modifier(Modifier::BOLD);
    let complete_style = Style::default()
        .fg(palette.success_fg)
        .add_modifier(Modifier::BOLD);
    let future_style = Style::default().fg(palette.muted_fg);
    let label_style = Style::default().fg(palette.primary_fg);

    FLOW_BOOTSTRAP_PHASES
        .iter()
        .enumerate()
        .map(|(phase_index, phase)| {
            let title = format!("    Phase {}: {}", phase_index + 1, phase.title);
            let title_padding = title_width.saturating_sub(title.chars().count());
            let status_label = flow_phase_status_label(flow, phase_index);
            let status_text = status_label
                .map(|label| format!("[{label}]"))
                .unwrap_or_default();
            let status_padding = status_width.saturating_sub(status_text.chars().count());
            let mut spans = vec![
                Span::styled(title, label_style),
                Span::styled(
                    " ".repeat(title_padding + TITLE_STATUS_GAP),
                    future_style,
                ),
                Span::styled(status_text, status_style),
                Span::styled(
                    " ".repeat(status_padding + STATUS_ANIM_GAP),
                    future_style,
                ),
            ];
            let (start, _) = flow_phase_bounds(phase_index);
            for (step_offset, _) in phase.steps.iter().enumerate() {
                if step_offset > 0 {
                    spans.push(Span::raw(" "));
                }
                let step_index = start + step_offset;
                match flow_phase_step_state(flow, step_index) {
                    FlowPhaseStepState::Future => {
                        spans.push(Span::styled("✧", future_style));
                    }
                    FlowPhaseStepState::Pending => {
                        spans.push(Span::styled("✧", pending_style));
                    }
                    FlowPhaseStepState::Complete => {
                        spans.push(Span::styled("✦", complete_style));
                    }
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn build_animation_snapshot(app: &AppState) -> Vec<String> {
    if app.session_flows.is_empty() {
        return Vec::new();
    }
    let now_millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut rows = Vec::new();
    for flow in app
        .session_flows
        .iter()
        .filter(|flow| should_display_flow_row(flow, app.remote_connected))
    {
        let latest_action = latest_flow_action(flow);
        let closing = flow.closing_started_ms.is_some();
        let lane_active = closing
            || !flow.anim_queue.is_empty()
            || (app.server_running && app.ngrok_running && app.remote_connected);
        let direction = flow_direction(Some(flow), now_millis);
        let phase = flow_phase(flow, now_millis);
        let lit = flow_lit_count(
            Some(flow),
            lane_active,
            now_millis,
            direction,
            0,
            0,
            0,
            FLOW_ROW_CELLS,
        );
        let lane = debug_lane(direction, lit, FLOW_ROW_CELLS);
        rows.push(format!(
            "sid {} phase={:<8} tool={:<16} Your computer {} ChatGPT Web (via Ngrok)",
            flow.short_id, phase, latest_action, lane
        ));
    }
    if rows.is_empty() {
        return Vec::new();
    }
    rows
}

fn clipboard_copy(text: &str) {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let _ = stdout().write_all(seq.as_bytes());
    let _ = stdout().flush();
    let text_owned = text.to_string();
    std::thread::spawn(move || {
        for (cmd, args) in [
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ] {
            if let Ok(mut child) = std::process::Command::new(cmd)
                .args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(text_owned.as_bytes());
                }
                let _ = child.wait();
                return;
            }
        }
    });
}

// ── Main ────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3200);
    let workspace_root = match std::env::var("WORKSPACE_ROOT") {
        Ok(path) => path,
        Err(_) => std::env::current_dir()?.to_string_lossy().into_owned(),
    };

    let state: SharedState = Arc::new(Mutex::new(AppState::new(port, workspace_root)?));

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, state.clone()).await;

    stdout().execute(DisableBracketedPaste)?;
    stdout().execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    // Cleanup
    {
        let mut app = state.lock().await;
        if let Some(handle) = app.ngrok_task.take() {
            handle.abort();
        }
        if let Some(ref mut child) = app.remote_browser_child {
            let _ = child.kill().await;
        }
        if let Some(ref mut child) = app.devtools_child {
            let _ = child.kill().await;
        }
    }

    result
}

// ── Phase 1: Mode selection ─────────────────────────────────

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<(), Box<dyn std::error::Error>> {
    // Draw mode selection screen
    loop {
        let (current_theme, current_tool_mode) = {
            let app = state.lock().await;
            (app.current_theme(), app.tool_mode)
        };
        terminal.draw(|f| draw_mode_select(f, current_theme, current_tool_mode))?;

        if event::poll(UI_POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let mode = match key.code {
                    KeyCode::Char('1') => Mode::Computer,
                    KeyCode::Char('2') => Mode::Browser,
                    KeyCode::Char('3') => Mode::Both,
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('s') => {
                        run_settings(terminal, state.clone()).await?;
                        continue;
                    }
                    _ => continue,
                };
                {
                    let mut app = state.lock().await;
                    app.mode = mode;
                    app.log("INFO", format!("Mode: {}", mode.label()));
                    app.persist_state_with_log();
                }
                break;
            }
        }
    }

    if mode_is_browser_enabled(state.clone()).await {
        let continue_run = run_browser_select(terminal, state.clone()).await?;
        if !continue_run {
            return Ok(());
        }
    }

    let continue_run = run_ngrok_auth_setup(terminal, state.clone()).await?;
    if !continue_run {
        return Ok(());
    }

    // Start services
    let devtools_bridge = start_services(state.clone()).await;

    // Phase 2: main TUI loop
    run_tui(terminal, state, devtools_bridge).await
}

fn draw_mode_select(f: &mut Frame, theme: &theme::ThemeDef, tool_mode: ToolMode) {
    let palette = theme.palette;
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Length(16), // Mode selection
            Constraint::Min(0),     // Spacer
        ])
        .split(area);

    let header =
        Paragraph::new("  CatDesk - MCP Tools for ChatGPT to control your computer and browser")
            .style(
                Style::default()
                    .fg(palette.header_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(palette.border_type)
                    .border_style(Style::default().fg(palette.border_fg)),
            );
    f.render_widget(header, chunks[0]);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Select mode:",
            Style::default()
                .fg(palette.title_fg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [1] ",
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Control Computer   ",
                Style::default().fg(palette.primary_fg),
            ),
            Span::styled("(run_command)", Style::default().fg(palette.muted_fg)),
        ]),
        Line::from(vec![
            Span::styled(
                "  [2] ",
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Control Browser    ",
                Style::default().fg(palette.primary_fg),
            ),
            Span::styled(
                "(chrome-devtools-mcp)",
                Style::default().fg(palette.muted_fg),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  [3] ",
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Both", Style::default().fg(palette.primary_fg)),
        ]),
        Line::from(vec![
            Span::styled(
                "  [s] ",
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Settings", Style::default().fg(palette.primary_fg)),
            Span::styled(
                format!(
                    " (theme: {}, tool mode: {})",
                    theme.label,
                    tool_mode.label()
                ),
                Style::default().fg(palette.muted_fg),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [q] ", Style::default().fg(palette.danger_fg)),
            Span::styled("Quit", Style::default().fg(palette.muted_fg)),
        ]),
    ];

    let select = Paragraph::new(lines).block(
        Block::default()
            .title(" Mode ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(select, chunks[1]);
}

async fn run_ngrok_auth_setup(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<bool, Box<dyn std::error::Error>> {
    if load_ngrok_authtoken()?.is_some() {
        return Ok(true);
    }

    let config_path = app_config_path()?;
    let config_path_text = config_path.to_string_lossy().into_owned();
    let mut input = String::new();
    let mut error_message: Option<String> = None;

    loop {
        let (
            current_theme,
            current_tool_mode,
            current_mode,
            browsers,
            selected_browser,
        ) = {
            let app = state.lock().await;
            (
                app.current_theme(),
                app.tool_mode,
                app.mode,
                app.detected_browsers.clone(),
                app.selected_browser.clone(),
            )
        };
        let supported_indices: Vec<usize> = browsers
            .iter()
            .enumerate()
            .filter(|(_, browser)| browser.mcp_supported)
            .map(|(idx, _)| idx)
            .collect();
        let selected_supported_idx =
            selected_supported_browser_idx(&browsers, selected_browser.as_ref());
        terminal.draw(|f| {
            let anchor_area = if current_mode.browser_enabled() {
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(10),
                        Constraint::Length(3),
                    ])
                    .split(f.area())[1]
            } else {
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Length(16),
                        Constraint::Min(0),
                    ])
                    .split(f.area())[1]
            };
            if current_mode.browser_enabled() {
                draw_browser_select(
                    f,
                    &browsers,
                    &supported_indices,
                    selected_supported_idx,
                    current_theme,
                );
            } else {
                draw_mode_select(f, current_theme, current_tool_mode);
            }
            draw_ngrok_auth_setup(
                f,
                current_theme,
                anchor_area,
                &config_path_text,
                &masked_secret_preview(&input),
                error_message.as_deref(),
            )
        })?;

        if !event::poll(UI_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
                    KeyCode::Enter => {
                        let token = input.trim();
                        if token.is_empty() {
                            error_message = Some("NGROK_AUTHTOKEN cannot be empty".into());
                            continue;
                        }
                        match save_ngrok_authtoken(token) {
                            Ok(saved_path) => {
                                let mut app = state.lock().await;
                                app.log(
                                    "INFO",
                                    format!(
                                        "Saved ngrok authtoken to {}",
                                        saved_path.to_string_lossy()
                                    ),
                                );
                                return Ok(true);
                            }
                            Err(e) => {
                                error_message =
                                    Some(format!("Failed to save ~/.catdesk/config.toml: {e}"));
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        input.pop();
                        error_message = None;
                    }
                    KeyCode::Char(c) => {
                        input.push(c);
                        error_message = None;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                input.push_str(&text);
                error_message = None;
            }
            _ => {}
        }
    }
}

fn masked_secret_preview(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = value.chars().collect();
    let visible = chars.len().min(4);
    let masked_len = chars.len().saturating_sub(visible);
    let mut preview = "*".repeat(masked_len);
    preview.extend(chars[chars.len() - visible..].iter());
    preview
}

fn draw_ngrok_auth_setup(
    f: &mut Frame,
    theme: &theme::ThemeDef,
    anchor_area: Rect,
    _config_path: &str,
    masked_value: &str,
    error_message: Option<&str>,
) {
    let palette = theme.palette;
    let modal_bg = Color::Rgb(34, 38, 47);
    let modal_fg = Color::Rgb(232, 236, 242);

    let modal_area = centered_rect(72, 10, anchor_area);
    f.render_widget(Clear, modal_area);
    let modal_block = Block::default()
        .title(" ngrok auth ")
        .borders(Borders::ALL)
        .border_type(palette.border_type)
        .border_style(Style::default().fg(palette.border_fg))
        .style(Style::default().bg(modal_bg));
    let modal_inner = modal_block.inner(modal_area);
    f.render_widget(modal_block, modal_area);

    let modal_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(modal_inner);

    let body_lines = vec![
        Line::from(Span::styled(
            "ngrok setup required",
            Style::default()
                .fg(palette.title_fg)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Open https://dashboard.ngrok.com/get-started/setup to get your authtoken."),
        Line::from("Paste the token from `ngrok config add-authtoken ...` below."),
    ];
    let body = Paragraph::new(body_lines)
        .style(Style::default().fg(modal_fg).bg(modal_bg))
        .wrap(Wrap { trim: false });
    f.render_widget(body, modal_chunks[0]);

    let input_line = if masked_value.is_empty() {
        "_".to_string()
    } else {
        masked_value.to_string()
    };
    let input = Paragraph::new(format!("  {input_line}"))
        .style(Style::default().fg(palette.title_fg).bg(modal_bg))
        .block(
            Block::default()
                .title(" NGROK_AUTHTOKEN ")
                .borders(Borders::ALL)
                .border_type(palette.border_type)
                .border_style(Style::default().fg(palette.border_fg))
                .style(Style::default().bg(modal_bg)),
        );
    f.render_widget(input, modal_chunks[1]);

    let footer = if let Some(message) = error_message {
        Paragraph::new(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(palette.danger_fg).bg(modal_bg),
        )))
    } else {
        Paragraph::new(Line::from(Span::styled(
            "[Enter] Save  [q/Esc] Quit  [Paste] Insert token",
            Style::default().fg(palette.muted_fg).bg(modal_bg),
        )))
    };
    f.render_widget(footer, modal_chunks[2]);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(percent_x).saturating_div(100).max(44);
    let width = width.min(area.width.saturating_sub(2).max(1));
    let popup_height = height.min(area.height.saturating_sub(2).max(1));
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    Rect::new(x, y, width, popup_height)
}

async fn run_settings(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<(), Box<dyn std::error::Error>> {
    let themes = theme::all();
    let tool_modes = ToolMode::all();
    let mut confirm_reset_token_billing = false;
    let mut selected_row = {
        let app = state.lock().await;
        themes.iter().position(|t| t.id == app.theme).unwrap_or(0)
    };
    let total_rows = themes.len() + tool_modes.len();

    loop {
        let (current_theme, current_tool_mode, usage_totals) = {
            let app = state.lock().await;
            (app.current_theme(), app.tool_mode, app.usage_totals.clone())
        };
        terminal.draw(|f| {
            draw_settings(
                f,
                current_theme,
                current_tool_mode,
                &usage_totals,
                selected_row,
                confirm_reset_token_billing,
            )
        })?;

        if event::poll(UI_POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Up => {
                        confirm_reset_token_billing = false;
                        selected_row = selected_row.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        confirm_reset_token_billing = false;
                        if selected_row + 1 < total_rows {
                            selected_row += 1;
                        }
                    }
                    KeyCode::Enter => {
                        confirm_reset_token_billing = false;
                        let mut app = state.lock().await;
                        if selected_row < themes.len() {
                            let picked = themes[selected_row];
                            if app.theme != picked.id {
                                app.theme = picked.id.to_string();
                                app.log("INFO", format!("Theme changed to {}", picked.label));
                                app.persist_state_with_log();
                            }
                        } else {
                            let picked = tool_modes[selected_row - themes.len()];
                            if app.tool_mode != picked {
                                app.tool_mode = picked;
                                app.log("INFO", format!("Tool mode: {}", picked.label()));
                                app.persist_state_with_log();
                            }
                        }
                    }
                    KeyCode::Char('r') => {
                        if !confirm_reset_token_billing {
                            confirm_reset_token_billing = true;
                            continue;
                        }
                        let mut app = state.lock().await;
                        app.usage_totals = UsageTotals::default();
                        app.log("INFO", "Token billing totals reset".into());
                        app.persist_state_with_log();
                        confirm_reset_token_billing = false;
                    }
                    _ => {
                        confirm_reset_token_billing = false;
                    }
                }
            }
        }
    }
}

fn draw_settings(
    f: &mut Frame,
    current_theme: &theme::ThemeDef,
    current_tool_mode: ToolMode,
    usage_totals: &UsageTotals,
    selected_row: usize,
    confirm_reset_token_billing: bool,
) {
    let themes = theme::all();
    let tool_modes = ToolMode::all();
    let palette = current_theme.palette;
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let header = Paragraph::new("  Settings")
        .style(
            Style::default()
                .fg(palette.header_fg)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(palette.border_type)
                .border_style(Style::default().fg(palette.border_fg)),
        );
    f.render_widget(header, chunks[0]);

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Choose a theme:",
            Style::default()
                .fg(palette.title_fg)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    for (idx, theme) in themes.iter().enumerate() {
        let selected = idx == selected_row;
        let marker = if selected { ">" } else { " " };
        let name_style = if selected {
            Style::default()
                .fg(palette.key_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.primary_fg)
        };
        lines.push(Line::from(""));
        let mut spans = vec![Span::styled(
            format!(" {} [{}] {}", marker, idx + 1, theme.label),
            name_style,
        )];
        if theme.id == current_theme.id {
            spans.push(Span::styled(
                "  [current]",
                Style::default()
                    .fg(palette.secondary_fg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(vec![Span::styled(
            format!("     {}", theme.description),
            Style::default().fg(palette.muted_fg),
        )]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "  Choose a tool mode:",
        Style::default()
            .fg(palette.title_fg)
            .add_modifier(Modifier::BOLD),
    )]));
    for (idx, tool_mode) in tool_modes.iter().enumerate() {
        let row_idx = themes.len() + idx;
        let selected = row_idx == selected_row;
        let marker = if selected { ">" } else { " " };
        let name_style = if selected {
            Style::default()
                .fg(palette.key_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.primary_fg)
        };
        lines.push(Line::from(""));
        let mut spans = vec![Span::styled(
            format!(" {} [{}] {}", marker, row_idx + 1, tool_mode.label()),
            name_style,
        )];
        if *tool_mode == current_tool_mode {
            spans.push(Span::styled(
                "  [current]",
                Style::default()
                    .fg(palette.secondary_fg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(vec![Span::styled(
            format!("     {}", tool_mode.description()),
            Style::default().fg(palette.muted_fg),
        )]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Token billing:",
        Style::default()
            .fg(palette.title_fg)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::styled("  Input: ", Style::default().fg(palette.muted_fg)),
        Span::styled(
            usage_totals.input_tokens.to_string(),
            Style::default().fg(palette.primary_fg),
        ),
        Span::styled("   Output: ", Style::default().fg(palette.muted_fg)),
        Span::styled(
            usage_totals.output_tokens.to_string(),
            Style::default().fg(palette.primary_fg),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Total: ", Style::default().fg(palette.muted_fg)),
        Span::styled(
            usage_totals.total_tokens.to_string(),
            Style::default()
                .fg(palette.secondary_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("   Tool calls: ", Style::default().fg(palette.muted_fg)),
        Span::styled(
            usage_totals.tool_call_count.to_string(),
            Style::default().fg(palette.primary_fg),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  [r]", Style::default().fg(palette.warning_fg)),
        Span::styled(
            if confirm_reset_token_billing {
                " Press again to confirm token billing reset"
            } else {
                " Reset token billing totals"
            },
            Style::default().fg(if confirm_reset_token_billing {
                palette.danger_fg
            } else {
                palette.muted_fg
            }),
        ),
    ]));

    let body = Paragraph::new(lines).block(
        Block::default()
            .title(" Theme, Tool Mode & Billing ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(body, chunks[1]);

    let keys = Paragraph::new(Line::from(vec![
        Span::styled("  [Up/Down]", Style::default().fg(palette.key_fg)),
        Span::raw(" Select  "),
        Span::styled("[Enter]", Style::default().fg(palette.success_fg)),
        Span::raw(" Apply  "),
        Span::styled(
            "[r]",
            Style::default().fg(if confirm_reset_token_billing {
                palette.danger_fg
            } else {
                palette.warning_fg
            }),
        ),
        Span::raw(if confirm_reset_token_billing {
            " Confirm reset  "
        } else {
            " Reset token billing  "
        }),
        Span::styled("[q/Esc]", Style::default().fg(palette.danger_fg)),
        Span::raw(" Back"),
    ]))
    .block(
        Block::default()
            .title(" Keys ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(keys, chunks[2]);
}

async fn mode_is_browser_enabled(state: SharedState) -> bool {
    state.lock().await.mode.browser_enabled()
}

fn browser_identity_matches(
    browser: &browser::DetectedBrowser,
    selected: &browser::DetectedBrowser,
) -> bool {
    browser.path == selected.path && browser.binary == selected.binary
}

fn selected_supported_browser_idx(
    browsers: &[browser::DetectedBrowser],
    selected_browser: Option<&browser::DetectedBrowser>,
) -> usize {
    let supported_indices: Vec<usize> = browsers
        .iter()
        .enumerate()
        .filter(|(_, browser)| browser.mcp_supported)
        .map(|(idx, _)| idx)
        .collect();
    if supported_indices.is_empty() {
        return 0;
    }
    let Some(selected_browser) = selected_browser else {
        return 0;
    };
    let Some(browser_idx) = browsers
        .iter()
        .position(|browser| browser_identity_matches(browser, selected_browser))
    else {
        return 0;
    };
    supported_indices
        .iter()
        .position(|idx| *idx == browser_idx)
        .unwrap_or(0)
}

async fn run_browser_select(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut browsers = browser::detect_browsers();
    let mut selected_supported_idx = {
        let mut app = state.lock().await;
        app.detected_browsers = browsers.clone();
        let selected_missing = app.selected_browser.as_ref().is_some_and(|selected| {
            !browsers
                .iter()
                .any(|browser| browser_identity_matches(browser, selected))
        });
        if selected_missing {
            app.selected_browser = None;
            app.persist_state_with_log();
        }
        selected_supported_browser_idx(&browsers, app.selected_browser.as_ref())
    };
    loop {
        let supported_indices: Vec<usize> = browsers
            .iter()
            .enumerate()
            .filter(|(_, b)| b.mcp_supported)
            .map(|(idx, _)| idx)
            .collect();
        if !supported_indices.is_empty() {
            selected_supported_idx =
                selected_supported_idx.min(supported_indices.len().saturating_sub(1));
        } else {
            selected_supported_idx = 0;
        }

        let current_theme = {
            let app = state.lock().await;
            app.current_theme()
        };
        terminal.draw(|f| {
            draw_browser_select(
                f,
                &browsers,
                &supported_indices,
                selected_supported_idx,
                current_theme,
            )
        })?;

        if event::poll(UI_POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => return Ok(false),
                    KeyCode::Char('r') => {
                        browsers = browser::detect_browsers();
                        let mut app = state.lock().await;
                        app.detected_browsers = browsers.clone();
                        let selected_missing =
                            app.selected_browser.as_ref().is_some_and(|selected| {
                                !browsers
                                    .iter()
                                    .any(|browser| browser_identity_matches(browser, selected))
                            });
                        if selected_missing {
                            app.selected_browser = None;
                            app.persist_state_with_log();
                        }
                        selected_supported_idx = selected_supported_browser_idx(
                            &browsers,
                            app.selected_browser.as_ref(),
                        );
                    }
                    KeyCode::Up => {
                        selected_supported_idx = selected_supported_idx.saturating_sub(1)
                    }
                    KeyCode::Down => {
                        if selected_supported_idx + 1 < supported_indices.len() {
                            selected_supported_idx += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(selected_idx) = supported_indices.get(selected_supported_idx) {
                            if let Some(selected) = browsers.get(*selected_idx).cloned() {
                                persist_selected_browser(state.clone(), selected).await;
                                return Ok(true);
                            }
                        }
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        let index = c.to_digit(10).unwrap_or(0) as usize;
                        if index == 0 {
                            continue;
                        }
                        let target_idx = index - 1;
                        if let Some(browser_idx) = supported_indices.get(target_idx) {
                            if let Some(selected) = browsers.get(*browser_idx).cloned() {
                                persist_selected_browser(state.clone(), selected).await;
                                return Ok(true);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn persist_selected_browser(state: SharedState, selected: browser::DetectedBrowser) {
    let remote_info = selected
        .remote_debug_target
        .as_deref()
        .unwrap_or("not active");
    let mut app = state.lock().await;
    app.selected_browser = Some(selected.clone());
    app.log(
        "INFO",
        format!(
            "Selected browser: {} ({}, {})",
            selected.name, selected.binary, selected.path
        ),
    );
    app.log(
        "INFO",
        format!("Selected browser remote debugging: {remote_info}"),
    );
    app.persist_state_with_log();
}

fn draw_browser_select(
    f: &mut Frame,
    browsers: &[browser::DetectedBrowser],
    supported_indices: &[usize],
    selected_supported_idx: usize,
    theme: &theme::ThemeDef,
) {
    let palette = theme.palette;
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(area);

    let header = Paragraph::new("  Select Browser - Installed and Remote Debugging Status")
        .style(
            Style::default()
                .fg(palette.header_fg)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(palette.border_type)
                .border_style(Style::default().fg(palette.border_fg)),
        );
    f.render_widget(header, chunks[0]);

    let active_summary = browser::format_active_remote_debug_names(browsers);
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                "  Installed browsers: ",
                Style::default().fg(palette.muted_fg),
            ),
            Span::styled(
                browsers.len().to_string(),
                Style::default()
                    .fg(palette.title_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Remote debugging active: ",
                Style::default().fg(palette.muted_fg),
            ),
            Span::styled(active_summary, Style::default().fg(palette.success_fg)),
        ]),
        Line::from(vec![
            Span::styled(
                "  Selectable (Chromium): ",
                Style::default().fg(palette.muted_fg),
            ),
            Span::styled(
                supported_indices.len().to_string(),
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];

    if browsers.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No browser found in PATH. Press [r] to rescan, [q] to quit.",
            Style::default().fg(palette.danger_fg),
        )));
    } else if supported_indices.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Only unsupported browsers found (e.g. Firefox). Chromium browsers are required.",
            Style::default().fg(palette.danger_fg),
        )));
        lines.push(Line::from(""));
        for browser in browsers {
            lines.push(Line::from(vec![Span::styled(
                format!("   [x] {} ({})", browser.name, browser.binary),
                Style::default().fg(palette.muted_fg),
            )]));
            lines.push(Line::from(vec![Span::styled(
                format!("     status: {}", browser.support_note),
                Style::default().fg(palette.warning_fg),
            )]));
            lines.push(Line::from(""));
        }
    } else {
        let selected_browser_index = supported_indices
            .get(selected_supported_idx)
            .copied()
            .unwrap_or(supported_indices[0]);
        for (idx, browser) in browsers.iter().enumerate() {
            let selected = idx == selected_browser_index;
            let prefix = if selected { ">" } else { " " };
            let quick_pick_num = supported_indices
                .iter()
                .position(|candidate_idx| *candidate_idx == idx)
                .map(|v| v + 1);
            let title_style = if !browser.mcp_supported {
                Style::default().fg(palette.muted_fg)
            } else if selected {
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.primary_fg)
            };
            if let Some(num) = quick_pick_num {
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        " {} [{}] {} ({})",
                        prefix, num, browser.name, browser.binary
                    ),
                    title_style,
                )]));
            } else {
                lines.push(Line::from(vec![Span::styled(
                    format!("   [x] {} ({})", browser.name, browser.binary),
                    title_style,
                )]));
            }
            lines.push(Line::from(vec![Span::styled(
                format!("     path: {}", browser.path),
                Style::default().fg(palette.muted_fg),
            )]));
            lines.push(Line::from(vec![Span::styled(
                format!("     status: {}", browser.support_note),
                Style::default().fg(if browser.mcp_supported {
                    palette.success_fg
                } else {
                    palette.warning_fg
                }),
            )]));
            if !browser.mcp_supported {
                lines.push(Line::from(vec![Span::styled(
                    "     remote debugging integration: not supported yet",
                    Style::default().fg(palette.warning_fg),
                )]));
            } else if browser.remote_debug_active {
                let target = browser.remote_debug_target.as_deref().unwrap_or("unknown");
                let pid = browser
                    .remote_debug_pid
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "--".into());
                lines.push(Line::from(vec![Span::styled(
                    format!("     remote debugging: ACTIVE at {target} (pid {pid})"),
                    Style::default().fg(palette.success_fg),
                )]));
            } else {
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "     remote debugging: not active (supported flag: {})",
                        browser.remote_debug_hint
                    ),
                    Style::default().fg(palette.warning_fg),
                )]));
            }
            lines.push(Line::from(""));
        }
    }

    let body = Paragraph::new(lines).block(
        Block::default()
            .title(" Browser List ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(body, chunks[1]);

    let keys = Paragraph::new(Line::from(vec![
        Span::styled("  [Up/Down]", Style::default().fg(palette.key_fg)),
        Span::raw(" Select  "),
        Span::styled("[1-9]", Style::default().fg(palette.key_fg)),
        Span::raw(" Quick select (Chromium only)  "),
        Span::styled("[Enter]", Style::default().fg(palette.success_fg)),
        Span::raw(" Confirm  "),
        Span::styled("[r]", Style::default().fg(palette.warning_fg)),
        Span::raw(" Rescan  "),
        Span::styled("[q]", Style::default().fg(palette.danger_fg)),
        Span::raw(" Quit"),
    ]))
    .block(
        Block::default()
            .title(" Keys ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(keys, chunks[2]);
}

fn find_available_remote_debug_port(start: u16, end: u16) -> Option<u16> {
    (start..=end).find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
}

fn sanitize_for_filename(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if sanitized.is_empty() {
        "browser".into()
    } else {
        sanitized
    }
}

async fn wait_remote_debug_ready(port: u16, timeout: Duration) -> bool {
    let client = reqwest::Client::new();
    let endpoint = format!("http://127.0.0.1:{port}/json/version");
    let started = Instant::now();
    while started.elapsed() < timeout {
        let result = client
            .get(&endpoint)
            .timeout(Duration::from_millis(600))
            .send()
            .await;
        if let Ok(response) = result {
            if response.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

async fn ensure_selected_browser_remote_debugging(
    state: SharedState,
    selected_browser: Option<browser::DetectedBrowser>,
) -> Option<browser::DetectedBrowser> {
    let Some(mut selected) = selected_browser else {
        return None;
    };
    if !selected.mcp_supported {
        state.lock().await.log(
            "ERROR",
            format!(
                "Selected browser {} is not supported yet for chrome-devtools-mcp",
                selected.name
            ),
        );
        return None;
    }
    if selected.remote_debug_active && selected.remote_debug_target.is_some() {
        return Some(selected);
    }

    let Some(port) = find_available_remote_debug_port(9222, 9322) else {
        state.lock().await.log(
            "ERROR",
            "No available local port in range 9222-9322 for remote debugging".into(),
        );
        return Some(selected);
    };

    let user_data_dir = format!(
        "/tmp/catdesk-remote-debug-{}",
        sanitize_for_filename(&selected.binary)
    );
    if let Err(e) = std::fs::create_dir_all(&user_data_dir) {
        state.lock().await.log(
            "WARN",
            format!("Failed to create user data dir {user_data_dir}: {e}"),
        );
    }

    let mut command = tokio::process::Command::new(&selected.path);
    command
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--user-data-dir={user_data_dir}"))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            state.lock().await.log(
                "ERROR",
                format!(
                    "Failed to launch {} with remote debugging: {}",
                    selected.name, e
                ),
            );
            return Some(selected);
        }
    };
    let launched_pid = child.id();

    let existing_child = {
        let mut app = state.lock().await;
        app.remote_browser_child.take()
    };
    if let Some(mut old_child) = existing_child {
        let _ = old_child.kill().await;
    }

    {
        let mut app = state.lock().await;
        app.remote_browser_child = Some(child);
        app.log(
            "INFO",
            format!(
                "Launched {} with remote debugging on 127.0.0.1:{}",
                selected.name, port
            ),
        );
    }

    if wait_remote_debug_ready(port, Duration::from_secs(10)).await {
        selected.remote_debug_active = true;
        selected.remote_debug_target = Some(format!("127.0.0.1:{port}"));
        selected.remote_debug_pid = launched_pid;
        {
            let mut app = state.lock().await;
            app.selected_browser = Some(selected.clone());
            app.log(
                "INFO",
                format!(
                    "Remote debugging ready for {} at 127.0.0.1:{}",
                    selected.name, port
                ),
            );
            app.persist_state_with_log();
        }
        Some(selected)
    } else {
        state.lock().await.log(
            "WARN",
            format!(
                "Remote debugging endpoint for {} did not become ready in time",
                selected.name
            ),
        );
        Some(selected)
    }
}

// ── Start services ──────────────────────────────────────────

async fn start_services(state: SharedState) -> Option<Arc<Mutex<DevtoolsBridge>>> {
    let (port, mode, mut detected_browsers, mut selected_browser) = {
        let app = state.lock().await;
        (
            app.port,
            app.mode,
            app.detected_browsers.clone(),
            app.selected_browser.clone(),
        )
    };

    if mode.browser_enabled() && detected_browsers.is_empty() {
        detected_browsers = browser::detect_browsers();
    }
    if mode.browser_enabled() {
        selected_browser =
            ensure_selected_browser_remote_debugging(state.clone(), selected_browser).await;
        detected_browsers = browser::detect_browsers();
        if let Some(selected) = &selected_browser {
            if let Some(refreshed) = detected_browsers
                .iter()
                .find(|b| b.path == selected.path && b.binary == selected.binary)
                .cloned()
            {
                selected_browser = Some(refreshed);
            }
        }
        let mut app = state.lock().await;
        app.detected_browsers = detected_browsers.clone();
        app.selected_browser = selected_browser.clone();
        app.persist_state_with_log();
    }

    let browser_summary = browser::format_browser_names(&detected_browsers);
    let remote_support_summary = browser::format_remote_debug_names(&detected_browsers);
    let remote_active_summary = browser::format_active_remote_debug_names(&detected_browsers);
    let browser_details: Vec<String> = detected_browsers
        .iter()
        .map(|b| {
            format!(
                "Browser: {} (binary: {}, path: {}, support: {}, remote debug flag: {}, remote debug active: {}, pid: {})",
                b.name,
                b.binary,
                b.path,
                b.support_note,
                b.remote_debug_hint,
                b.remote_debug_target.as_deref().unwrap_or("no"),
                b.remote_debug_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "--".into())
            )
        })
        .collect();
    {
        let mut app = state.lock().await;
        app.detected_browsers = detected_browsers;
        if browser_summary == "--" {
            app.log("WARN", "No local browser found in PATH".into());
        } else {
            app.log("INFO", format!("Local browsers: {browser_summary}"));
        }
        if remote_support_summary == "--" {
            app.log(
                "WARN",
                "No detected browser supports remote debugging".into(),
            );
        } else {
            app.log(
                "INFO",
                format!("Remote debugging supported: {remote_support_summary}"),
            );
        }
        if remote_active_summary == "--" {
            app.log(
                "WARN",
                "No browser currently runs with remote debugging".into(),
            );
        } else {
            app.log(
                "INFO",
                format!("Remote debugging active: {remote_active_summary}"),
            );
        }
        if mode.browser_enabled() {
            if let Some(selected) = &selected_browser {
                let target = selected
                    .remote_debug_target
                    .as_deref()
                    .unwrap_or("launch new browser instance");
                app.log(
                    "INFO",
                    format!(
                        "Using browser: {} ({}) -> {}",
                        selected.name, selected.path, target
                    ),
                );
            } else {
                app.log("WARN", "No browser was selected before startup".into());
            }
        }
        for detail in browser_details {
            app.log("INFO", detail);
        }
    }

    // Start MCP HTTP server
    let devtools_bridge = if mode.browser_enabled() {
        if selected_browser.is_none() {
            state.lock().await.log(
                "ERROR",
                "Browser mode requires selecting a supported Chromium browser".into(),
            );
            None
        } else {
            state
                .lock()
                .await
                .log("INFO", "Starting chrome-devtools-mcp...".into());
            match DevtoolsBridge::start(selected_browser.as_ref()).await {
                Ok(bridge) => {
                    let mut app = state.lock().await;
                    app.devtools_running = true;
                    app.log("INFO", "chrome-devtools-mcp started".into());
                    Some(bridge)
                }
                Err(e) => {
                    let mut app = state.lock().await;
                    app.log("ERROR", format!("chrome-devtools-mcp: {e}"));
                    None
                }
            }
        }
    } else {
        None
    };

    let mcp_path = {
        let app = state.lock().await;
        app.mcp_path()
    };
    let router = server::router(state.clone(), devtools_bridge.clone(), mcp_path);
    let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await {
        Ok(l) => l,
        Err(e) => {
            state
                .lock()
                .await
                .log("ERROR", format!("Failed to bind port {port}: {e}"));
            return devtools_bridge;
        }
    };

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    {
        let mut app = state.lock().await;
        app.server_running = true;
        app.server_handle = Some(handle);
        app.log("INFO", format!("MCP Server started on port {port}"));
    }

    // Start ngrok
    if let Err(e) = ngrok::start(state.clone()).await {
        state.lock().await.log("ERROR", format!("ngrok: {e}"));
    }

    devtools_bridge
}

// ── Phase 2: Main TUI ──────────────────────────────────────

async fn run_tui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
    _devtools: Option<Arc<Mutex<DevtoolsBridge>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut log_scroll: usize = 0;
    let mut log_follow_tail = true;
    let mut last_log_max_scroll: usize = 0;
    let mut last_log_effective_scroll: usize = 0;
    let mut selection = Selection::new();
    // (message, position (col, row), created_at)
    let mut toast: Option<(&str, (u16, u16), Instant)> = None;
    #[allow(unused_assignments)]
    let mut screen_lines: Vec<String> = vec![];
    let mut last_animation_snapshot = String::new();

    loop {
        {
            let mut app = state.lock().await;
            app.prune_closed_session_flows();
        }
        {
            let app = state.lock().await;
            let toast_ref = toast
                .as_ref()
                .filter(|(_, _, t)| t.elapsed().as_secs() < 2)
                .map(|(m, pos, _)| (*m, *pos));
            let mut new_lines: Vec<String> = Vec::new();
            let mut latest_log_view: Option<(usize, usize)> = None;
            terminal.draw(|f| {
                draw_ui(
                    f,
                    &app,
                    log_scroll,
                    log_follow_tail,
                    &mut latest_log_view,
                    toast_ref,
                );

                if let Some(((c0, r0), (c1, r1))) = selection.range() {
                    let palette = app.current_theme().palette;
                    let area = f.area();
                    for row in r0..=r1 {
                        if row >= area.height {
                            break;
                        }
                        let cs = if row == r0 { c0 } else { 0 };
                        let ce = if row == r1 {
                            c1
                        } else {
                            area.width.saturating_sub(1)
                        };
                        for col in cs..=ce {
                            if col >= area.width {
                                break;
                            }
                            if let Some(cell) = f.buffer_mut().cell_mut((col, row)) {
                                cell.set_style(
                                    Style::default()
                                        .bg(palette.selection_bg)
                                        .fg(palette.selection_fg),
                                );
                            }
                        }
                    }
                }

                let area = f.area();
                let buf = f.buffer_mut();
                for row in 0..area.height {
                    let mut line = String::new();
                    for col in 0..area.width {
                        line.push_str(buf[(col, row)].symbol());
                    }
                    new_lines.push(line);
                }
            })?;
            if let Some((max_scroll, effective_scroll)) = latest_log_view {
                last_log_max_scroll = max_scroll;
                last_log_effective_scroll = effective_scroll;
                if !log_follow_tail && log_scroll > last_log_max_scroll {
                    log_scroll = last_log_max_scroll;
                }
            }
            screen_lines = new_lines;
        }

        let snapshots = {
            let app = state.lock().await;
            build_animation_snapshot(&app)
        };
        if !snapshots.is_empty() {
            let snapshot_joined = snapshots.join("\n");
            if snapshot_joined != last_animation_snapshot {
                last_animation_snapshot = snapshot_joined;
            }
        }

        if let Some((_, _, t)) = &toast {
            if t.elapsed().as_secs() >= 2 {
                toast = None;
            }
        }

        if event::poll(UI_POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    selection.clear();
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Up => {
                            if log_follow_tail {
                                log_follow_tail = false;
                                log_scroll = last_log_effective_scroll.saturating_sub(1);
                            } else {
                                log_scroll = log_scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if !log_follow_tail {
                                log_scroll = (log_scroll + 1).min(last_log_max_scroll);
                                if log_scroll >= last_log_max_scroll {
                                    log_follow_tail = true;
                                }
                            }
                        }
                        KeyCode::End => {
                            log_follow_tail = true;
                            log_scroll = last_log_max_scroll;
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        selection.start = Some((mouse.column, mouse.row));
                        selection.end = Some((mouse.column, mouse.row));
                        selection.dragging = true;
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if selection.dragging {
                            selection.end = Some((mouse.column, mouse.row));
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if selection.dragging {
                            selection.end = Some((mouse.column, mouse.row));
                            selection.dragging = false;
                            if let Some((start, end)) = selection.range() {
                                if start != end {
                                    let text = extract_from_screen(&screen_lines, start, end);
                                    if !text.is_empty() {
                                        clipboard_copy(&text);
                                        toast = Some((
                                            "Copied!",
                                            (mouse.column, mouse.row),
                                            Instant::now(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if log_follow_tail {
                            log_follow_tail = false;
                            log_scroll = last_log_effective_scroll.saturating_sub(1);
                        } else {
                            log_scroll = log_scroll.saturating_sub(1);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if !log_follow_tail {
                            log_scroll = (log_scroll + 1).min(last_log_max_scroll);
                            if log_scroll >= last_log_max_scroll {
                                log_follow_tail = true;
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    // Stop services
    {
        let mut app = state.lock().await;
        if let Some(handle) = app.server_handle.take() {
            handle.abort();
        }
        app.server_running = false;
    }
    let _ = ngrok::stop(state.clone()).await;
    {
        let mut app = state.lock().await;
        app.remote_connected = false;
        app.last_remote_activity_ms = None;
    }

    Ok(())
}

// ── Draw main UI ────────────────────────────────────────────

fn draw_ui(
    f: &mut Frame,
    app: &AppState,
    log_scroll: usize,
    log_follow_tail: bool,
    log_view: &mut Option<(usize, usize)>,
    toast: Option<(&str, (u16, u16))>,
) {
    let palette = app.current_theme().palette;
    let area = f.area();
    let now_millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let both_running = app.server_running && app.ngrok_running;
    let has_url = app.ngrok_url.is_some();
    let visible_flow_count = app
        .session_flows
        .iter()
        .filter(|flow| should_display_flow_row(flow, app.remote_connected))
        .count() as u16;
    let within_connect_grace = app
        .last_remote_activity_ms
        .map(|t| now_millis.saturating_sub(t) < REMOTE_CONNECT_UI_GRACE_MS)
        .unwrap_or(false);
    let show_guide = both_running
        && has_url
        && !app.remote_connected
        && visible_flow_count == 0
        && !within_connect_grace;
    let show_session_flow = !show_guide;
    let flow_lines = if show_session_flow {
        if visible_flow_count == 0 {
            2
        } else {
            visible_flow_count.saturating_mul(2)
        }
    } else {
        0
    };
    let mascot_block_height = app.mascot.required_tui_block_height();
    let desired_status_height = if show_guide { 20 } else { 17 + flow_lines };
    let desired_status_height = desired_status_height.max(mascot_block_height);
    let logs_min_height = if show_guide { 3 } else { 5 };
    let max_status_height = area.height.saturating_sub(6 + logs_min_height).max(17);
    let status_height = desired_status_height.min(max_status_height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(status_height),
            Constraint::Length(3),
            Constraint::Min(logs_min_height),
        ])
        .split(area);

    // ── Header ──
    let header =
        Paragraph::new("  CatDesk - MCP Tools for ChatGPT to control your computer and browser")
            .style(
                Style::default()
                    .fg(palette.header_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(palette.border_type)
                    .border_style(Style::default().fg(palette.border_fg)),
            );
    f.render_widget(header, chunks[0]);

    // ── Status ──
    let mode_label = app.mode.label();
    let tool_mode_label = app.tool_mode.label();
    let server_status = if app.server_running {
        format!("RUNNING (port {})", app.port)
    } else {
        "STOPPED".into()
    };
    let ngrok_status: &str = if app.ngrok_running {
        "RUNNING"
    } else {
        "STOPPED"
    };
    let devtools_status: &str = if app.devtools_running {
        "RUNNING"
    } else {
        if app.mode.browser_enabled() {
            "STOPPED"
        } else {
            "N/A"
        }
    };
    let mcp_url: String = app.public_mcp_url().unwrap_or_else(|| "--".into());
    let browser_summary = browser::format_browser_names(&app.detected_browsers);
    let remote_support_summary = browser::format_remote_debug_names(&app.detected_browsers);
    let remote_active_summary = browser::format_active_remote_debug_names(&app.detected_browsers);
    let selected_browser_summary = app
        .selected_browser
        .as_ref()
        .map(|b| format!("{} ({})", b.name, b.binary))
        .unwrap_or_else(|| "--".into());
    let selected_target_summary = app
        .selected_browser
        .as_ref()
        .map(|b| {
            b.remote_debug_target
                .clone()
                .unwrap_or_else(|| "launch new browser instance".into())
        })
        .unwrap_or_else(|| "--".into());
    let trim_line = |text: &str, max_chars: usize| -> String {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= max_chars {
            return text.to_string();
        }
        let kept = chars[..max_chars.saturating_sub(3)]
            .iter()
            .collect::<String>();
        format!("{kept}...")
    };
    let computer_role_style = Style::default()
        .fg(if app.server_running {
            palette.success_fg
        } else {
            palette.muted_fg
        })
        .add_modifier(Modifier::BOLD);
    let chatgpt_role_style = Style::default()
        .fg(if app.remote_connected {
            palette.success_fg
        } else {
            palette.muted_fg
        })
        .add_modifier(Modifier::BOLD);
    let flow_meta_style = Style::default()
        .fg(palette.info_fg)
        .add_modifier(Modifier::BOLD);
    let lane_left_padding = "Your computer ".len();
    let call_offset_for = |text: &str| -> String {
        let text_width = text.chars().count();
        let centered_in_lane = FLOW_ROW_CELLS.saturating_sub(text_width) / 2;
        " ".repeat(lane_left_padding + centered_in_lane)
    };
    let lane_for = |active: bool, flow: Option<&SessionFlow>| -> Vec<Span<'static>> {
        const DASHES: usize = FLOW_ROW_CELLS - 1;
        const CELLS: usize = FLOW_ROW_CELLS;
        let unlit = Style::default().fg(palette.muted_fg);
        let lit = Style::default()
            .fg(palette.info_fg)
            .add_modifier(Modifier::BOLD);

        if !active && flow.is_none() {
            return vec![
                Span::styled(format!("{} ", "-".repeat(DASHES)), unlit),
                Span::raw(" "),
            ];
        }

        let direction = flow_direction(flow, now_millis);
        let lit_count = flow_lit_count(flow, active, now_millis, direction, 0, 0, 0, CELLS);

        let mut spans = Vec::with_capacity(CELLS + 1);
        for i in 0..CELLS {
            let lit_here = match direction {
                FlowDirection::Forward => lit_count > 0 && i < lit_count,
                FlowDirection::Backward => lit_count > 0 && i >= CELLS.saturating_sub(lit_count),
            };
            let ch = match direction {
                FlowDirection::Forward => {
                    if i == DASHES {
                        '>'
                    } else {
                        '-'
                    }
                }
                FlowDirection::Backward => {
                    if i == 0 {
                        '<'
                    } else {
                        '-'
                    }
                }
            };
            let style = if lit_here { lit } else { unlit };
            spans.push(Span::styled(ch.to_string(), style));
        }
        spans.push(Span::raw(" "));
        spans
    };
    let session_stats_for = |app: &AppState| -> Vec<Span<'static>> {
        vec![
            Span::styled("  Sessions: ", Style::default().fg(palette.muted_fg)),
            Span::styled(
                app.session_count.to_string(),
                Style::default().fg(palette.title_fg),
            ),
            Span::styled("    Requests: ", Style::default().fg(palette.muted_fg)),
            Span::styled(
                app.request_count.to_string(),
                Style::default().fg(palette.title_fg),
            ),
        ]
    };
    let session_meta_gap_for = |sid_text: &str| -> String {
        const SID_COLUMN_WIDTH: usize = 16;
        " ".repeat(SID_COLUMN_WIDTH.saturating_sub(sid_text.chars().count()).max(2))
    };

    let mut status_lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                "  Mode:             ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                mode_label,
                Style::default()
                    .fg(palette.secondary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Tool mode:        ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                tool_mode_label,
                Style::default()
                    .fg(palette.secondary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Server:           ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &server_status,
                Style::default().fg(if app.server_running {
                    palette.success_fg
                } else {
                    palette.danger_fg
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  ngrok:            ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                ngrok_status,
                Style::default().fg(if app.ngrok_running {
                    palette.success_fg
                } else {
                    palette.danger_fg
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  DevTools:         ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                devtools_status,
                Style::default().fg(if app.devtools_running {
                    palette.success_fg
                } else {
                    palette.muted_fg
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  MCP Server URL:   ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &mcp_url,
                Style::default().fg(if has_url {
                    palette.info_fg
                } else {
                    palette.muted_fg
                }),
            ),
        ]),
        {
            let mut spans = vec![Span::styled(
                "  Remote connected: ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            )];
            if app.remote_connected {
                spans.push(Span::styled(
                    "V",
                    Style::default()
                        .fg(palette.success_fg)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    "X",
                    Style::default()
                        .fg(palette.danger_fg)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        },
    ];

    if !show_guide {
        status_lines.push(Line::from(vec![
            Span::styled(
                "  Local browsers:   ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(browser_summary, Style::default().fg(palette.title_fg)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  Remote dbg support:",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(remote_support_summary, Style::default().fg(palette.info_fg)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  Remote dbg active: ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                remote_active_summary,
                Style::default().fg(palette.success_fg),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  Selected browser: ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                selected_browser_summary,
                Style::default().fg(palette.secondary_fg),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  Selected target:  ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                selected_target_summary,
                Style::default().fg(palette.info_fg),
            ),
        ]));
    }

    if show_session_flow {
        status_lines.push(Line::from(""));
        if visible_flow_count == 0 {
            let call_text = if app.remote_connected {
                "awaiting request"
            } else {
                "session closed"
            };
            let call_offset = call_offset_for(call_text);
            status_lines.push(Line::from(vec![
                Span::styled("    ", Style::default().fg(palette.muted_fg)),
                Span::styled(call_offset, Style::default().fg(palette.muted_fg)),
                Span::styled(call_text, flow_meta_style),
            ]));
            let lane = lane_for(false, None);
            let sid_text = if app.remote_connected {
                "[sid=idle]"
            } else {
                "[sid=closed]"
            };
            let mut row = vec![
                Span::styled("    ", Style::default().fg(palette.muted_fg)),
                Span::styled("Your computer ", computer_role_style),
            ];
            row.extend(lane);
            row.push(Span::styled("ChatGPT Web", chatgpt_role_style));
            row.push(Span::styled("  ", Style::default().fg(palette.muted_fg)));
            row.push(Span::styled(sid_text, flow_meta_style));
            row.push(Span::styled(
                session_meta_gap_for(sid_text),
                Style::default().fg(palette.muted_fg),
            ));
            row.extend(session_stats_for(app));
            status_lines.push(Line::from(row));
            status_lines.extend(flow_phase_lines(None, &palette, flow_meta_style));
        } else {
            for flow in app
                .session_flows
                .iter()
                .filter(|flow| should_display_flow_row(flow, app.remote_connected))
            {
                let latest_action = latest_flow_action(flow);
                let call_text = trim_line(&format!("call {latest_action}"), FLOW_ROW_CELLS);
                let call_offset = call_offset_for(&call_text);
                status_lines.push(Line::from(vec![
                    Span::styled("    ", Style::default().fg(palette.muted_fg)),
                    Span::styled(call_offset, Style::default().fg(palette.muted_fg)),
                    Span::styled(call_text, flow_meta_style),
                ]));
                let closing = flow.closing_started_ms.is_some();
                let lane_active = closing
                    || !flow.anim_queue.is_empty()
                    || (app.server_running && app.ngrok_running && app.remote_connected);
                let lane = lane_for(lane_active, Some(flow));
                let sid_text = format!("[sid={}]", flow.short_id);
                let sid_gap = session_meta_gap_for(&sid_text);
                let mut row = vec![
                    Span::styled("    ", Style::default().fg(palette.muted_fg)),
                    Span::styled("Your computer ", computer_role_style),
                ];
                row.extend(lane);
                row.push(Span::styled("ChatGPT Web", chatgpt_role_style));
                row.push(Span::styled("  ", Style::default().fg(palette.muted_fg)));
                row.push(Span::styled(sid_text, flow_meta_style));
                row.push(Span::styled(
                    sid_gap,
                    Style::default().fg(palette.muted_fg),
                ));
                row.extend(session_stats_for(app));
                status_lines.push(Line::from(row));
                status_lines.extend(flow_phase_lines(Some(flow), &palette, flow_meta_style));
            }
        }
    }

    let guide_lines = if show_guide {
        vec![
            Line::from(vec![
                Span::styled(
                    "1. ",
                    Style::default()
                        .fg(palette.title_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Open connector settings:",
                    Style::default().fg(palette.primary_fg),
                ),
            ]),
            Line::from(vec![
                Span::styled("   ", Style::default().fg(palette.primary_fg)),
                Span::styled(
                    "https://chatgpt.com/apps#settings/Connectors",
                    Style::default()
                        .fg(palette.primary_fg)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    "2. ",
                    Style::default()
                        .fg(palette.title_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("Click ", Style::default().fg(palette.primary_fg)),
                Span::styled(
                    "Create app",
                    Style::default()
                        .fg(palette.primary_fg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    "3. ",
                    Style::default()
                        .fg(palette.title_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("Fill in the form:", Style::default().fg(palette.primary_fg)),
            ]),
            Line::from(vec![Span::styled(
                "   Name: CatDesk (or any name you like)",
                Style::default().fg(palette.secondary_fg),
            )]),
            Line::from(vec![Span::styled(
                format!("   MCP Server URL: {mcp_url}"),
                Style::default()
                    .fg(palette.info_fg)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![Span::styled(
                "   Authentication: None",
                Style::default().fg(palette.secondary_fg),
            )]),
            Line::from(vec![
                Span::styled(
                    "4. ",
                    Style::default()
                        .fg(palette.title_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Click \"I understand and want to continue\"",
                    Style::default().fg(palette.primary_fg),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    "5. ",
                    Style::default()
                        .fg(palette.title_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("Click Create", Style::default().fg(palette.primary_fg)),
            ]),
        ]
    } else {
        Vec::new()
    };

    if !show_guide {
        status_lines.push(Line::from(""));
        status_lines.push(Line::from(vec![
            Span::styled("  Workspace: ", Style::default().fg(palette.muted_fg)),
            Span::styled(
                &*app.workspace_root,
                Style::default().fg(palette.primary_fg),
            ),
        ]));
    }

    let status_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(42)])
        .split(chunks[1]);
    let status_block = Block::default()
        .title(" Status ")
        .borders(Borders::ALL)
        .border_type(palette.border_type)
        .border_style(Style::default().fg(palette.border_fg));
    let status_inner = status_block.inner(status_columns[0]);
    f.render_widget(status_block, status_columns[0]);
    let mascot_block = Block::default()
        .title(" Binagotchy ")
        .borders(Borders::ALL)
        .border_type(palette.border_type)
        .border_style(Style::default().fg(palette.border_fg));
    let mascot_inner = mascot_block.inner(status_columns[1]);
    f.render_widget(mascot_block, status_columns[1]);
    let mascot = Paragraph::new(render_tui_lines(
        app.mascot.current_tui_frame(now_millis),
        mascot_inner.height,
    ))
    .alignment(Alignment::Center);
    f.render_widget(mascot, mascot_inner);

    if show_guide {
        let top_height = (status_lines.len() as u16).min(status_inner.height.saturating_sub(1));
        let guide_height =
            (guide_lines.len() as u16 + 2).min(status_inner.height.saturating_sub(top_height));
        let status_parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(top_height),
                Constraint::Length(guide_height),
                Constraint::Min(0),
            ])
            .split(status_inner);
        let status_summary = Paragraph::new(status_lines);
        f.render_widget(status_summary, status_parts[0]);

        let guide_bg = if app.theme == "neon" {
            Color::Rgb(48, 16, 54)
        } else {
            Color::Rgb(58, 72, 98)
        };
        let guide_panel = Paragraph::new(guide_lines)
            .style(Style::default().bg(guide_bg))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" What to do next? ")
                    .borders(Borders::ALL)
                    .border_type(palette.border_type)
                    .border_style(
                        Style::default()
                            .fg(palette.info_fg)
                            .bg(guide_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
            );
        f.render_widget(guide_panel, status_parts[1]);
    } else {
        let status = Paragraph::new(status_lines);
        f.render_widget(status, status_inner);
    }

    // ── Keys ──
    let key_spans = vec![
        Span::styled("  [q]", Style::default().fg(palette.danger_fg)),
        Span::raw(" Quit  "),
        Span::styled("[Up/Down]", Style::default().fg(palette.key_fg)),
        Span::raw(" Scroll logs  "),
        Span::styled("[Wheel]", Style::default().fg(palette.key_fg)),
        Span::raw(" Scroll logs  "),
        Span::styled("[End]", Style::default().fg(palette.key_fg)),
        Span::raw(" Follow latest"),
    ];
    let keys = Paragraph::new(Line::from(key_spans)).block(
        Block::default()
            .title(" Keys ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(keys, chunks[2]);

    // ── Logs ──
    let log_items: Vec<ListItem> = app
        .logs
        .iter()
        .map(|entry| {
            let color = match entry.level {
                "ERROR" => palette.danger_fg,
                "WARN" => palette.warning_fg,
                _ => palette.muted_fg,
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", entry.time),
                    Style::default().fg(palette.muted_fg),
                ),
                Span::styled(format!("{:5} ", entry.level), Style::default().fg(color)),
                Span::styled(&*entry.message, Style::default().fg(palette.primary_fg)),
            ]))
        })
        .collect();

    let visible_height = chunks[3].height.saturating_sub(2) as usize;
    let total = log_items.len();
    let max_scroll = total.saturating_sub(visible_height);
    let effective_scroll = if log_follow_tail {
        max_scroll
    } else {
        log_scroll.min(max_scroll)
    };
    *log_view = Some((max_scroll, effective_scroll));
    let visible_items: Vec<ListItem> = log_items
        .into_iter()
        .skip(effective_scroll)
        .take(visible_height)
        .collect();
    let logs = List::new(visible_items).block(
        Block::default()
            .title(" Logs ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(logs, chunks[3]);

    // ── Floating toast ──
    if let Some((msg, (col, row))) = toast {
        let label = format!(" {msg} ");
        let w = label.len() as u16;
        let x = col.saturating_add(1).min(area.width.saturating_sub(w));
        let y = if row > 0 { row - 1 } else { row + 1 }.min(area.height.saturating_sub(1));
        let toast_area = Rect::new(x, y, w, 1);
        let toast_widget = Paragraph::new(label).style(
            Style::default()
                .bg(palette.toast_bg)
                .fg(palette.toast_fg)
                .add_modifier(Modifier::BOLD),
        );
        f.render_widget(toast_widget, toast_area);
    }
}

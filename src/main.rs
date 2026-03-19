mod browser;
mod command;
mod devtools;
mod mcp;
mod ngrok;
mod server;
mod state;
mod theme;
mod workspace_tools;

use crossterm::{
    ExecutableCommand,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use devtools::DevtoolsBridge;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use state::{AppState, Mode, SharedState, ToolMode};
use std::io::{Write, stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

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
    let workspace_root = std::env::var("WORKSPACE_ROOT")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| "/tmp".into());

    let state: SharedState = Arc::new(Mutex::new(AppState::new(port, workspace_root)));

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, state.clone()).await;

    stdout().execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    // Cleanup
    {
        let mut app = state.lock().await;
        if let Some(ref mut child) = app.ngrok_child {
            let _ = child.kill().await;
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

        if event::poll(std::time::Duration::from_millis(100))? {
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
                        run_theme_settings(terminal, state.clone()).await?;
                        continue;
                    }
                    KeyCode::Char('t') => {
                        let mut app = state.lock().await;
                        app.tool_mode = app.tool_mode.next();
                        let tool_mode_label = app.tool_mode.label();
                        app.log("INFO", format!("Tool mode: {tool_mode_label}"));
                        continue;
                    }
                    _ => continue,
                };
                {
                    let mut app = state.lock().await;
                    app.mode = mode;
                    app.log("INFO", format!("Mode: {}", mode.label()));
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
        Paragraph::new("  MCP3000 - MCP Tools for ChatGPT to control your computer and browser")
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
                format!(" (theme: {})", theme.label),
                Style::default().fg(palette.muted_fg),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  [t] ",
                Style::default()
                    .fg(palette.key_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Tool mode", Style::default().fg(palette.primary_fg)),
            Span::styled(
                format!(" ({})", tool_mode.label()),
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

async fn run_theme_settings(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<(), Box<dyn std::error::Error>> {
    let themes = theme::all();
    let mut selected_idx = {
        let app = state.lock().await;
        themes.iter().position(|t| t.id == app.theme).unwrap_or(0)
    };

    loop {
        let current_theme = {
            let app = state.lock().await;
            app.current_theme()
        };
        terminal.draw(|f| draw_theme_settings(f, current_theme, selected_idx))?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Up => selected_idx = selected_idx.saturating_sub(1),
                    KeyCode::Down => {
                        if selected_idx + 1 < themes.len() {
                            selected_idx += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let picked = themes[selected_idx];
                        let mut app = state.lock().await;
                        app.theme = picked.id.to_string();
                        app.log("INFO", format!("Theme changed to {}", picked.label));
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

fn draw_theme_settings(f: &mut Frame, current_theme: &theme::ThemeDef, selected_idx: usize) {
    let themes = theme::all();
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

    let header = Paragraph::new("  Settings - Theme")
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
        let selected = idx == selected_idx;
        let marker = if selected { ">" } else { " " };
        let name_style = if selected {
            Style::default()
                .fg(palette.key_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.primary_fg)
        };
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            format!(" {} [{}] {}", marker, idx + 1, theme.label),
            name_style,
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("     {}", theme.description),
            Style::default().fg(palette.muted_fg),
        )]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Current: ", Style::default().fg(palette.muted_fg)),
        Span::styled(
            current_theme.label,
            Style::default()
                .fg(palette.secondary_fg)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let body = Paragraph::new(lines).block(
        Block::default()
            .title(" Theme ")
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

async fn run_browser_select(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    state: SharedState,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut browsers = browser::detect_browsers();
    {
        let mut app = state.lock().await;
        app.detected_browsers = browsers.clone();
        app.selected_browser = None;
    }

    let mut selected_supported_idx: usize = 0;
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

        if event::poll(std::time::Duration::from_millis(100))? {
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
        "/tmp/mcp3000-remote-debug-{}",
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

    let router = server::router(state.clone(), devtools_bridge.clone());
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
    let mut selection = Selection::new();
    // (message, position (col, row), created_at)
    let mut toast: Option<(&str, (u16, u16), Instant)> = None;
    #[allow(unused_assignments)]
    let mut screen_lines: Vec<String> = vec![];

    loop {
        {
            let app = state.lock().await;
            let toast_ref = toast
                .as_ref()
                .filter(|(_, _, t)| t.elapsed().as_secs() < 2)
                .map(|(m, pos, _)| (*m, *pos));
            let mut new_lines: Vec<String> = Vec::new();
            terminal.draw(|f| {
                draw_ui(f, &app, log_scroll, toast_ref);

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
            screen_lines = new_lines;
        }

        if let Some((_, _, t)) = &toast {
            if t.elapsed().as_secs() >= 2 {
                toast = None;
            }
        }

        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    selection.clear();
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Up => log_scroll = log_scroll.saturating_sub(1),
                        KeyCode::Down => log_scroll = log_scroll.saturating_add(1),
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
    state.lock().await.remote_connected = false;

    Ok(())
}

// ── Draw main UI ────────────────────────────────────────────

fn draw_ui(f: &mut Frame, app: &AppState, log_scroll: usize, toast: Option<(&str, (u16, u16))>) {
    let palette = app.current_theme().palette;
    let area = f.area();

    let both_running = app.server_running && app.ngrok_running;
    let has_url = app.ngrok_url.is_some();
    let show_guide = both_running && has_url && !app.remote_connected;
    let status_height = if show_guide { 24 } else { 17 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(status_height),
            Constraint::Length(3),
            Constraint::Min(5),
        ])
        .split(area);

    // ── Header ──
    let header =
        Paragraph::new("  MCP3000 - MCP Tools for ChatGPT to control your computer and browser")
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
    let mcp_url: String = app
        .ngrok_url
        .as_ref()
        .map(|u| format!("{u}/mcp"))
        .unwrap_or_else(|| "--".into());
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
        Line::from(vec![
            Span::styled(
                "  Local browsers:   ",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(browser_summary, Style::default().fg(palette.title_fg)),
        ]),
        Line::from(vec![
            Span::styled(
                "  Remote dbg support:",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(remote_support_summary, Style::default().fg(palette.info_fg)),
        ]),
        Line::from(vec![
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
        ]),
        Line::from(vec![
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
        ]),
        Line::from(vec![
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
        ]),
    ];

    if show_guide {
        status_lines.push(Line::from(""));
        status_lines.push(Line::from(Span::styled(
            "  What to do next?",
            Style::default()
                .fg(palette.title_fg)
                .add_modifier(Modifier::BOLD),
        )));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  1. ",
                Style::default()
                    .fg(palette.title_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Open connector settings: ",
                Style::default().fg(palette.primary_fg),
            ),
            Span::styled(
                "https://chatgpt.com/apps#settings/Connectors",
                Style::default()
                    .fg(palette.info_fg)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  2. ",
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
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  3. ",
                Style::default()
                    .fg(palette.title_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Fill in the form:", Style::default().fg(palette.primary_fg)),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "       Name:           ",
                Style::default().fg(palette.muted_fg),
            ),
            Span::styled(
                "MCP3000",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  (or any name you like)",
                Style::default().fg(palette.muted_fg),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "       MCP Server URL: ",
                Style::default().fg(palette.muted_fg),
            ),
            Span::styled(
                &mcp_url,
                Style::default()
                    .fg(palette.info_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "       Authentication: ",
                Style::default().fg(palette.muted_fg),
            ),
            Span::styled(
                "None",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  4. ",
                Style::default()
                    .fg(palette.title_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Click ", Style::default().fg(palette.primary_fg)),
            Span::styled(
                "\"I understand and want to continue\"",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled(
                "  5. ",
                Style::default()
                    .fg(palette.title_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Click ", Style::default().fg(palette.primary_fg)),
            Span::styled(
                "Create",
                Style::default()
                    .fg(palette.primary_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        status_lines.push(Line::from(""));
        status_lines.push(Line::from(vec![
            Span::styled("  Workspace: ", Style::default().fg(palette.muted_fg)),
            Span::styled(
                &*app.workspace_root,
                Style::default().fg(palette.primary_fg),
            ),
        ]));
        status_lines.push(Line::from(vec![
            Span::styled("  Sessions:  ", Style::default().fg(palette.muted_fg)),
            Span::styled(
                app.session_count.to_string(),
                Style::default().fg(palette.title_fg),
            ),
            Span::styled("    Requests: ", Style::default().fg(palette.muted_fg)),
            Span::styled(
                app.request_count.to_string(),
                Style::default().fg(palette.title_fg),
            ),
        ]));
    }

    let status = Paragraph::new(status_lines).block(
        Block::default()
            .title(" Status ")
            .borders(Borders::ALL)
            .border_type(palette.border_type)
            .border_style(Style::default().fg(palette.border_fg)),
    );
    f.render_widget(status, chunks[1]);

    // ── Keys ──
    let key_spans = vec![
        Span::styled("  [q]", Style::default().fg(palette.danger_fg)),
        Span::raw(" Quit  "),
        Span::styled("[Up/Down]", Style::default().fg(palette.key_fg)),
        Span::raw(" Scroll logs"),
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
    let effective_scroll = log_scroll.min(max_scroll);
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
